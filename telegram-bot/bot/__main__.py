import io
import json
import logging
import os
import signal
import sys
from enum import Enum
from pathlib import Path

import psycopg2
import toml
from PIL import Image
from redis import Redis
from telegram import Bot
from telegram.constants import PARSEMODE_MARKDOWN_V2
from telegram.ext import CommandHandler, MessageHandler, Updater
from telegram.ext.filters import Filters
from telegram.utils.request import Request


apos = "\u2019"
emoji_a = "\U0001F170\uFE0F"
emoji_b = "\U0001F171\uFE0F"

project_path = Path(os.getenv("STICKERDOME_DIR", Path(sys.path[0]).parent))

config = toml.load(project_path / "config.toml")

logging.basicConfig(filename=config["log_file"], level=logging.INFO)


class State(Enum):
    NOT_STARTED = b"not-started"
    TAKING_SUBMISSIONS = b"taking-submissions"
    VOTING = b"voting"
    ENDED = b"ended"


def _enum_values(enum):
    return {x.value for x in enum}


def _find_enum_by_value(enum, value):
    for x in enum:
        if x.value == value:
            return x
    return None


def _int_from_bytes(b):
    try:
        return int(b)
    except (TypeError, ValueError):
        return None


bot = Bot(
    token=config["api_token"],
    request=Request(con_pool_size=8),
)

updater = Updater(bot=bot)
dispatcher = updater.dispatcher

db_name = config.get("db_name", "stickerdome")
db = psycopg2.connect(f"dbname={db_name}")

with db:
    with open(project_path / "schema.sql") as schema:
        with db.cursor() as cur:
            cur.execute(schema.read())


redis = Redis(unix_socket_path=config["redis_socket"], db=config["redis_db"])


def reset():
    with db:
        with db.cursor() as cur:
            cur.execute("""DELETE FROM "submissions"; DELETE FROM "stickers"; DELETE FROM "users";""")
    redis.set("state", State.NOT_STARTED.value)
    for key in ["group_id", "current_stickers_message", "current_poll"]:
        redis.delete(key)


chat_id = _int_from_bytes(redis.get("group_id"))
if False and chat_id is not None:
    notice = bot.send_message(chat_id=chat_id, parse_mode=PARSEMODE_MARKDOWN_V2,
        text=f"""\
\u2022 *Removed* submission limit for stickers already posted\\.
\u2022 *Reset* everyone{apos}s submission limit for new stickers to 0/10\\.
""")
    bot.pin_chat_message(chat_id=chat_id, message_id=notice.message_id, disable_notification=True)


def exit_handler(signalnum, frame):
    db.close()
    redis.close()
    if (group_id := _int_from_bytes(redis.get("group_id"))) is not None:
        bot.send_message(chat_id=group_id, text="Stickerdome going down for maintenance and shit...")
    sys.exit(0)


signal.signal(signal.SIGINT, exit_handler)


def update_submission_data(*, new_transaction=True):
    data = []
    with db.cursor() as cur:
        cur.execute(
            """
            SELECT "stickers"."file_id", "stickers"."set", "temp"."count" FROM "stickers" JOIN (
                SELECT "sticker_id", count("sticker_id") AS "count" FROM "submissions"
                GROUP BY "sticker_id"
            ) AS "temp"
                ON "stickers"."id" = "temp"."sticker_id"
            """
        )
        for file_id, sticker_set, count in cur:
            data.append({"fileID": file_id, "stickerSet": sticker_set, "count": count})
    if new_transaction:
        db.commit()
    with open(project_path / "submissions.json", "w") as f:
        json.dump(data, f)


update_submission_data()


def upsert_sticker(sticker, user):
    with db.cursor() as cur:
        cur.execute(
            """
            INSERT INTO "stickers"("id", "file_id", "set", "width", "height", "submitter")
                VALUES (%s, %s, %s, %s, %s, %s)
                ON CONFLICT ("id") DO UPDATE SET
                    "file_id" = %s,
                    "set" = %s,
                    "width" = %s,
                    "height" = %s
            """,
            (
                sticker.file_unique_id,
                sticker.file_id,
                sticker.set_name,
                sticker.width,
                sticker.height,
                user.id,
                sticker.file_id,
                sticker.set_name,
                sticker.width,
                sticker.height
            )
        )
    db.commit()

    file_path = project_path / "stickers" / f"{sticker.file_id}.webp"
    if not file_path.is_file():
        with open(file_path, "wb") as f:
            bot.get_file(sticker.file_id).download(out=f)


def upsert_user(user):
    with db.cursor() as cur:
        cur.execute(
            """
            INSERT INTO "users"("id", "username") VALUES (%s, %s)
                ON CONFLICT ("id") DO UPDATE SET "username" = %s
            """,
            (user.id, user.username, user.username)
        )
    db.commit()


def add_submission(user, sticker):
    def get_user_submission_count(cur):
        cur.execute(
            """SELECT COUNT(*) FROM "stickers" WHERE "submitter" = %s""",
            (user.id,)
        )
        return cur.fetchone()[0]

    def get_sticker_submission_count(cur):
        cur.execute(
            """SELECT COUNT(*) FROM "submissions" WHERE "sticker_id" = %s""",
            (sticker.file_unique_id,)
        )
        return cur.fetchone()[0]

    max = config["max_submissions"]
    with db:
        with db.cursor() as cur:
            user_submissions = get_user_submission_count(cur)
            sticker_submissions = get_sticker_submission_count(cur)

            if sticker_submissions == 0 and user_submissions > max:
                return f"You{apos}ve reached your limit of {max} submissions."

            cur.execute(
                """SELECT COUNT(*) FROM "submissions" WHERE "user_id" = %s AND "sticker_id" = %s""",
                (user.id, sticker.file_unique_id)
            )
            user_sticker_submissions = cur.fetchone()[0]
            if user_sticker_submissions > 0:
                return f"You{apos}ve already submitted this sticker."

            cur.execute(
                """
                INSERT INTO "submissions"("user_id", "sticker_id") VALUES (%s, %s)
                    ON CONFLICT DO NOTHING
                """,
                (user.id, sticker.file_unique_id)
            )

            update_submission_data(new_transaction=False)

            sticker_submissions = get_sticker_submission_count(cur)
            if sticker_submissions == 0:
                return "Oops, something went wrong."
            elif sticker_submissions == 1:
                user_submissions = get_user_submission_count(cur)
                return f"Thanks for the new sticker! You have submitted {user_submissions}/{max} stickers."
            else:
                return f"Got it! This sticker has been submitted {sticker_submissions} times."


def sticker_message(update, context):
    message = update.message
    if message.reply_to_message:
        return

    user = message.from_user
    upsert_user(user)
    sticker = message.sticker
    if sticker.is_animated:
        message.reply_text(f"I{apos}m not prepared to deal with animated stickers!")
        return
    upsert_sticker(sticker, user)
    reply = add_submission(user, sticker)
    message.reply_text(reply)


sticker_chat_filter = Filters.chat()
sticker_handler = MessageHandler(
    callback=sticker_message,
    filters=Filters.sticker & sticker_chat_filter,
)
dispatcher.add_handler(sticker_handler)


def help_command(update, context):
    text = fr"Modeled after [XKCD{apos}s Emojidome](https://www.explainxkcd.com/wiki/index.php/2131:_Emojidome), Stickerdome aims to find the ultimate sticker by process of elimination\."

    state = redis.get("state")
    if state == State.TAKING_SUBMISSIONS.value:
        text += "\n\nCurrently in submission phase\\. The most submitted stickers advance to the voting phase\\."
    elif state == State.VOTING.value:
        text += "\n\nCurrently in voting phase\\. See the pinned message for the latest poll\\."
    elif state == State.ENDED.value:
        text += "\n\nThis Stickerdome has ended\\."

    with open(project_path / "stickerdome.png", "rb") as img:
        context.bot.send_photo(
            chat_id=update.effective_chat.id,
            photo=img,
            parse_mode=PARSEMODE_MARKDOWN_V2,
            caption=text,
        )


help_handler = CommandHandler(command="help", callback=help_command)
dispatcher.add_handler(help_handler)


def voting_command(update, context):
    if redis.get("state") != State.TAKING_SUBMISSIONS.value:
        update.effective_message.reply_text("The Stickerdome must be in submission phase to start voting.")
        return
    sticker_chat_filter.remove_chat_ids(update.effective_chat.id)
    redis.set("state", State.VOTING.value)
    context.bot.send_message(chat_id=update.effective_chat.id, text=f"Submissions closed, it{apos}s voting time!")
    change_poll(context, "AgADAwADZXlBFQ", "AgADyQUAApl_iAI")


voting_handler = CommandHandler(
    command="voting",
    callback=voting_command,
    filters=Filters.user(username=config["admins"]) & Filters.chat_type.groups,
)
dispatcher.add_handler(voting_handler)


def generate_versus_image(file_id_a, file_id_b, out):
    with Image.open(project_path / "stickers" / f"{file_id_a}.webp") as img_a, Image.open(project_path / "stickers" / f"{file_id_b}.webp") as img_b:
        img = Image.open(project_path / "versus-template.png")
        img.paste(img_a, ((512 - img_a.width) // 2, (512 - img_a.height) // 2 + 100))
        img.paste(img_b, ((512 - img_b.width) // 2 + 512 + 20, (512 - img_b.height) // 2 + 100))
        img.save(out, format="PNG")


def change_poll(context, sticker_id_a, sticker_id_b):
    bot = context.bot
    current_message_id = _int_from_bytes(redis.get("current_stickers_message"))
    current_poll_id = _int_from_bytes(redis.get("current_poll"))
    group_id = _int_from_bytes(redis.get("group_id"))

    if group_id is None:
        raise ValueError("Missing or invalid group_id")

    if current_poll_id is not None:
        bot.unpin_chat_message(chat_id=group_id, message_id=current_poll_id)
        bot.stop_poll(chat_id=group_id, message_id=current_poll_id)

    file_ids = []
    with db:
        with db.cursor() as cur:
            for sticker_unique_id in [sticker_id_a, sticker_id_b]:
                cur.execute("""SELECT "file_id" FROM "stickers" WHERE "id" = %s""", (sticker_unique_id,))
                file_ids.append(cur.fetchone()[0])

    with io.BytesIO() as img:
        generate_versus_image(*file_ids, img)
        stickers_message = bot.send_photo(chat_id=group_id, photo=img.getvalue())
    poll = bot.send_poll(chat_id=group_id, question="Which shall win?", options=[emoji_a, emoji_b], reply_to_message_id=stickers_message.message_id)
    bot.pin_chat_message(chat_id=group_id, message_id=poll.message_id)
    redis.set("current_stickers_message", stickers_message.message_id)
    redis.set("current_poll", poll.message_id)


def start_command(update, context):
    if update.effective_user.username not in config["admins"]:
        update.effective_message.reply_text("This bot can be only started by its admins.")
        return
    if update.effective_chat.type not in ["group", "supergroup"]:
        update.effective_message.reply_text("This bot can be only started in groups.")
        return

    chat_id = update.effective_chat.id
    if redis.get("state") != State.NOT_STARTED.value:
        context.bot.send_message(
            chat_id=chat_id,
            text="The Stickerdome has already begun!"
        )
        return
    redis.set("state", State.TAKING_SUBMISSIONS.value)
    redis.set("group_id", chat_id)
    sticker_chat_filter.add_chat_ids(chat_id)
    dispatcher.add_handler(sticker_handler)
    with open(project_path / "stickerdome.png", "rb") as img:
        welcome = context.bot.send_photo(
            chat_id=chat_id,
            photo=img,
            caption="The Stickerdome has started! Send your me dankest stickers!",
        )
    context.bot.pin_chat_message(
        chat_id=chat_id,
        message_id=welcome.message_id,
        disable_notification=True,
    )


start_handler = CommandHandler(command="start", callback=start_command)
dispatcher.add_handler(start_handler)


def stop_command(update, context):
    reset()
    context.bot.send_message(chat_id=update.effective_chat.id, text="The Stickerdome has been reset.")


stop_handler = CommandHandler(
    command="stop",
    callback=stop_command,
    filters=Filters.user(username=config["admins"]) & Filters.chat_type.groups,
)
dispatcher.add_handler(stop_handler)


state = redis.get("state")
group_id_bytes = redis.get("group_id")
group_id = _int_from_bytes(group_id_bytes)

if state not in _enum_values(State):
    raise ValueError("Invalid state in Redis")

if state == State.NOT_STARTED.value:
    if group_id_bytes is not None:
        raise ValueError("group_id should not exist in Redis")

if state in [State.TAKING_SUBMISSIONS.value, State.VOTING.value, State.ENDED.value]:
    if group_id is None:
        raise ValueError("Missing or invalid group_id in Redis")

if state == State.TAKING_SUBMISSIONS.value:
    for key in ["current_stickers_message", "current_poll"]:
        if redis.get(key) is not None:
            raise ValueError(f"{key} should not exist in Redis")
    sticker_chat_filter.add_chat_ids(group_id)

if state == State.VOTING.value:
    for key in ["current_stickers_message", "current_poll"]:
        if _int_from_bytes(redis.get(key)) is None:
            raise ValueError(f"Missing or invalid {key} in Redis")

if (group_id := _int_from_bytes(redis.get("group_id"))) is not None:
    bot.send_message(chat_id=group_id, text="The Stickerdome is back up! Sorry for the downtime.")

updater.start_webhook(
    listen="127.0.0.1",
    port=config["webhook_port"],
    webhook_url=config["webhook_url"],
)
