import json
import logging
import os
import signal
import sys
from enum import Enum
from pathlib import Path

import psycopg2
import toml
from redis import Redis
from telegram import Bot
from telegram.constants import PARSEMODE_MARKDOWN_V2
from telegram.ext import CommandHandler, MessageHandler, Updater
from telegram.ext.filters import Filters
from telegram.utils.request import Request


apos = "\u2019"

project_path = Path(os.getenv("STICKERDOME_DIR", Path(sys.path[0]).parent))

config = toml.load(project_path / "config.toml")

logging.basicConfig(filename=config["log_file"], level=logging.INFO)


class State(Enum):
    NOT_STARTED = b"not-started"
    TAKING_SUBMISSIONS = b"taking-submissions"
    VOTING = b"voting"
    ENDED = b"ended"


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
if not redis.get("state") or not redis.get("group_id"):
    redis.set("state", State.NOT_STARTED.value)

chat_id = redis.get("group_id")
if False and chat_id:
    chat_id = chat_id.decode()
    notice = bot.send_message(chat_id=chat_id, parse_mode=PARSEMODE_MARKDOWN_V2,
        text=f"""\
\u2022 *Removed* submission limit for stickers already posted\\.
\u2022 *Reset* everyone{apos}s submission limit for new stickers to 0/10\\.
""")
    bot.pin_chat_message(chat_id=chat_id, message_id=notice.message_id, disable_notification=True)

def exit_handler(signalnum, frame):
    db.close()
    redis.close()
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


chat_filter = Filters.chat()
sticker_handler = MessageHandler(
    callback=sticker_message,
    filters=Filters.sticker & chat_filter,
)
if redis.get("state") == State.TAKING_SUBMISSIONS.value:
    chat_filter.add_chat_ids(int(redis.get("group_id")))
    dispatcher.add_handler(sticker_handler)



def help_command(update, context):
    with open(project_path / "stickerdome.png", "rb") as img:
        context.bot.send_photo(
            chat_id=update.effective_chat.id,
            photo=img,
            parse_mode=PARSEMODE_MARKDOWN_V2,
            caption=fr"""Modeled after [XKCD{apos}s Emojidome](https://www.explainxkcd.com/wiki/index.php/2131:_Emojidome), Stickerdome aims to find the ultimate sticker by process of elimination\.

Currently in submission phase\. The most submitted stickers advance to the voting phase\.""",
        )


help_handler = CommandHandler(command="help", callback=help_command)
dispatcher.add_handler(help_handler)


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
    chat_filter.add_chat_ids(chat_id)
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

updater.start_webhook(
    listen="127.0.0.1",
    port=config["webhook_port"],
    webhook_url=config["webhook_url"],
)
