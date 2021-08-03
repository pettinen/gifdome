import io
import json
import logging
import os
import re
import signal
import secrets
import sys
from datetime import datetime
from enum import Enum
from pathlib import Path

import psycopg2
import toml
from PIL import Image
from redis import Redis
from telegram import Bot
from telegram.constants import PARSEMODE_MARKDOWN_V2
from telegram.error import BadRequest
from telegram.ext import CommandHandler, MessageHandler, PollHandler, Updater
from telegram.ext.filters import Filters
from telegram.utils.request import Request


project_path = Path(os.getenv("STICKERDOME_DIR", Path(sys.path[0]).parent))
config = toml.load(project_path / "config.toml")

DEBUG = config["debug"]["enabled"]

apos = "\u2019"
emoji_a = "\U0001F170\uFE0F"
emoji_b = "\U0001F171\uFE0F"

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


def _now():
    return int(datetime.now().timestamp())


bot = Bot(
    token=config["api_token"],
    request=Request(con_pool_size=16, read_timeout=15),
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
            cur.execute('DELETE FROM "submissions"')
    redis.set("state", State.NOT_STARTED.value)
    for key in [
        "group_id",
        "current_match",
        "current_stickers_message",
        "current_poll_message",
        "current_poll",
        "current_poll_start",
        "current_voter_count",
        "matches",
        #"seeding",
    ]:
        redis.delete(key)


def exit_handler(signalnum, frame):
    db.close()
    redis.close()
    if (
        config["downtime_notifications"]
        and (group_id := _int_from_bytes(redis.get("group_id"))) is not None
    ):
        bot.send_message(chat_id=group_id, text="Stickerdome going down for maintenance and shit...")
    sys.exit(0)


for signalnum in [signal.SIGINT, signal.SIGTERM]:
    signal.signal(signalnum, exit_handler)


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


def update_match_data():
    if (group_id := _int_from_bytes(redis.get("group_id"))) is None:
        return

    state = redis.get("state")
    if state == State.TAKING_SUBMISSIONS.value:
        description = "Send your dankest stickers!"
    elif state == State.VOTING.value:
        match_num = _int_from_bytes(redis.get("current_match"))
        if match_num < 128:
            round_of = "round of 256"
        elif match_num < 192:
            round_of = "round of 128"
        elif match_num < 224:
            round_of = "round of 64"
        elif match_num < 240:
            round_of = "round of 32"
        elif match_num < 248:
            round_of = "round of 16"
        elif match_num < 252:
            round_of = "quarterfinals"
        elif match_num < 254:
            round_of = "semifinals"
        elif match_num == 254:
            round_of = "the FINALE"
        else:
            round_of = f"wait, that shouldn{apos}t happen"
        description = f"Vote for the ultimate sticker!\nCurrent vote: {match_num + 1}/255 ({round_of})"
    elif state == State.ENDED.value:
        description = "This Stickerdome has ended."
    else:
        description = "The Stickerdome aims to find the ultimate sticker by process of elimination."
    try:
        bot.set_chat_description(chat_id=group_id, description=description)
    except BadRequest as e:
        if e.message != "Chat description is not modified":
            raise e

    data = []
    matches_raw = redis.get("matches")
    if matches_raw is not None:
        with db:
            with db.cursor() as cur:
                def get_file_id(sticker_id):
                    if sticker_id is None:
                        return sticker_id
                    cur.execute('SELECT "file_id" FROM "stickers" WHERE "id" = %s', (sticker_id,))
                    if (row := cur.fetchone()) is None:
                        return None
                    return row[0]

                matches = json.loads(matches_raw)
                for index, match in enumerate(matches):
                    match["participants"] = [
                        get_file_id(id) for id in match_participants(index, matches)
                    ]
                    match["winner"] = get_file_id(match["winner"])
                    data.append(match)
    with open(project_path / "matches.json", "w") as f:
        json.dump(data, f)


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

    # TODO: fetch and upsert sticker set

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
            'SELECT COUNT(*) FROM "stickers" WHERE "submitter" = %s',
            (user.id,)
        )
        return cur.fetchone()[0]

    def get_sticker_submission_count(cur):
        cur.execute(
            'SELECT COUNT(*) FROM "submissions" WHERE "sticker_id" = %s',
            (sticker.file_unique_id,)
        )
        return cur.fetchone()[0]

    with db:
        with db.cursor() as cur:
            user_submissions = get_user_submission_count(cur)
            sticker_submissions = get_sticker_submission_count(cur)

            cur.execute(
                'SELECT COUNT(*) FROM "submissions" WHERE "user_id" = %s AND "sticker_id" = %s',
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
                return f"Thanks for the new sticker! You have submitted {user_submissions} stickers."
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


def send_bracket(chat_id, caption=None, parse_mode=None):
    if not (project_path / "bracket.png").is_file():
        update_bracket_image()

    if caption is None:
        caption = r"High resolution version available at [stickerdome\.dipo\.rocks](https://stickerdome.dipo.rocks/)"
    if parse_mode is None:
        parse_mode = PARSEMODE_MARKDOWN_V2

    with Image.open(project_path / "bracket.png") as img:
        scale = 5000 / img.width
        smaller = img.resize(
            ((round(scale * img.width), round(scale * img.height))),
            resample = Image.LANCZOS,
        )
        with io.BytesIO() as f:
            smaller.save(f, "PNG")
            bot.send_photo(
                chat_id=chat_id,
                photo=f.getvalue(),
                caption=caption,
                parse_mode=parse_mode,
            )


def bracket_command(update, context):
    chat_id = update.effective_chat.id
    if update.effective_chat.type not in ["group", "supergroup"]:
        context.bot.send_message(
            chat_id=chat_id,
            text="The bracket is only available in groups.",
        )
        return

    state = redis.get("state")
    if state not in [State.VOTING.value, State.ENDED.value]:
        context.bot.send_message(
            chat_id=chat_id,
            text="The bracket is not available before the round of 64 in the voting phase."
        )
        return

    current_match_index = _int_from_bytes(redis.get("current_match"))
    if (
        current_match_index is None
        or (state == State.VOTING.value and current_match_index < 128)
    ):
        context.bot.send_message(
            chat_id=chat_id,
            text="The bracket is not available before the round of 128.",
        )
        return

    if not (project_path / "bracket.png").is_file():
        update_bracket_image()

    send_bracket(chat_id)


bracket_handler = CommandHandler(
    command="bracket",
    callback=bracket_command,
)
dispatcher.add_handler(bracket_handler)


def update_bracket_image():
    current_match_index = _int_from_bytes(redis.get("current_match"))
    if redis.get("matches") is None or current_match_index < 128:
        return

    coords = {128: (82, 82), 129: (82, 222), 130: (82, 362), 131: (82, 502), 132: (82, 642), 133: (82, 782), 134: (82, 922), 135: (82, 1062), 136: (82, 1202), 137: (82, 1342), 138: (82, 1482), 139: (82, 1622), 140: (82, 1762), 141: (82, 1902), 142: (82, 2042), 143: (82, 2182), 144: (82, 2322), 145: (82, 2462), 146: (82, 2602), 147: (82, 2742), 148: (82, 2882), 149: (82, 3022), 150: (82, 3162), 151: (82, 3302), 152: (82, 3442), 153: (82, 3582), 154: (82, 3722), 155: (82, 3862), 156: (82, 4002), 157: (82, 4142), 158: (82, 4282), 159: (82, 4422), 160: (6334, 82), 161: (6334, 222), 162: (6334, 362), 163: (6334, 502), 164: (6334, 642), 165: (6334, 782), 166: (6334, 922), 167: (6334, 1062), 168: (6334, 1202), 169: (6334, 1342), 170: (6334, 1482), 171: (6334, 1622), 172: (6334, 1762), 173: (6334, 1902), 174: (6334, 2042), 175: (6334, 2182), 176: (6334, 2322), 177: (6334, 2462), 178: (6334, 2602), 179: (6334, 2742), 180: (6334, 2882), 181: (6334, 3022), 182: (6334, 3162), 183: (6334, 3302), 184: (6334, 3442), 185: (6334, 3582), 186: (6334, 3722), 187: (6334, 3862), 188: (6334, 4002), 189: (6334, 4142), 190: (6334, 4282), 191: (6334, 4422), 192: (464, 152), 193: (464, 432), 194: (464, 712), 195: (464, 992), 196: (464, 1272), 197: (464, 1552), 198: (464, 1832), 199: (464, 2112), 200: (464, 2392), 201: (464, 2672), 202: (464, 2952), 203: (464, 3232), 204: (464, 3512), 205: (464, 3792), 206: (464, 4072), 207: (464, 4352), 208: (5952, 152), 209: (5952, 432), 210: (5952, 712), 211: (5952, 992), 212: (5952, 1272), 213: (5952, 1552), 214: (5952, 1832), 215: (5952, 2112), 216: (5952, 2392), 217: (5952, 2672), 218: (5952, 2952), 219: (5952, 3232), 220: (5952, 3512), 221: (5952, 3792), 222: (5952, 4072), 223: (5952, 4352), 224: (846, 292), 225: (846, 852), 226: (846, 1412), 227: (846, 1972), 228: (846, 2532), 229: (846, 3092), 230: (846, 3652), 231: (846, 4212), 232: (5570, 292), 233: (5570, 852), 234: (5570, 1412), 235: (5570, 1972), 236: (5570, 2532), 237: (5570, 3092), 238: (5570, 3652), 239: (5570, 4212), 240: (1099, 508), 241: (1099, 1628), 242: (1099, 2748), 243: (1099, 3868), 244: (5189, 508), 245: (5189, 1628), 246: (5189, 2748), 247: (5189, 3868), 248: (1611, 1068), 249: (1611, 3308), 250: (4677, 1068), 251: (4677, 3308), 252: (2180, 1528), 253: (3852, 2592), 254: (3016, 2060)}
    sizes = {128: 128, 129: 128, 130: 128, 131: 128, 132: 128, 133: 128, 134: 128, 135: 128, 136: 128, 137: 128, 138: 128, 139: 128, 140: 128, 141: 128, 142: 128, 143: 128, 144: 128, 145: 128, 146: 128, 147: 128, 148: 128, 149: 128, 150: 128, 151: 128, 152: 128, 153: 128, 154: 128, 155: 128, 156: 128, 157: 128, 158: 128, 159: 128, 160: 128, 161: 128, 162: 128, 163: 128, 164: 128, 165: 128, 166: 128, 167: 128, 168: 128, 169: 128, 170: 128, 171: 128, 172: 128, 173: 128, 174: 128, 175: 128, 176: 128, 177: 128, 178: 128, 179: 128, 180: 128, 181: 128, 182: 128, 183: 128, 184: 128, 185: 128, 186: 128, 187: 128, 188: 128, 189: 128, 190: 128, 191: 128, 192: 128, 193: 128, 194: 128, 195: 128, 196: 128, 197: 128, 198: 128, 199: 128, 200: 128, 201: 128, 202: 128, 203: 128, 204: 128, 205: 128, 206: 128, 207: 128, 208: 128, 209: 128, 210: 128, 211: 128, 212: 128, 213: 128, 214: 128, 215: 128, 216: 128, 217: 128, 218: 128, 219: 128, 220: 128, 221: 128, 222: 128, 223: 128, 224: 128, 225: 128, 226: 128, 227: 128, 228: 128, 229: 128, 230: 128, 231: 128, 232: 128, 233: 128, 234: 128, 235: 128, 236: 128, 237: 128, 238: 128, 239: 128, 240: 256, 241: 256, 242: 256, 243: 256, 244: 256, 245: 256, 246: 256, 247: 256, 248: 256, 249: 256, 250: 256, 251: 256, 252: 512, 253: 512, 254: 512}

    def pad_sticker(sticker):
        img = Image.new("RGBA", (512, 512))
        img.paste(sticker, ((512 - sticker.width) // 2, (512 - sticker.height) // 2))
        return img

    def resize_padded(sticker, size):
        return sticker.resize((size, size), resample=Image.LANCZOS)

    bracket = Image.open(project_path / "bracket-template.png")
    img = Image.new("RGBA", bracket.size)

    matches = json.loads(redis.get("matches"))
    with db:
        with db.cursor() as cur:
            for i, match in enumerate(matches):
                if i < 128 or match["winner"] is None:
                    continue
                cur.execute('SELECT "file_id" FROM "stickers" WHERE "id" = %s', (match["winner"],))
                file_id, = cur.fetchone()
                with Image.open(project_path / "stickers" / f"{file_id}.webp") as sticker:
                    sticker = resize_padded(pad_sticker(sticker), sizes[i])
                    img.paste(sticker, coords[i])
    img.paste(bracket, mask=bracket)
    img.save(project_path / "bracket.png")



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


def match_participants(index, matches):
    if index < 128:
        seeding = json.loads(redis.get("seeding"))
        return [seeding[index * 2], seeding[index * 2 + 1]]

    participants = []
    for match in matches:
        if match["next"] == index:
            participants.append(match["winner"])
    return participants


def generate_matches():
    next = [128, 160, 144, 176, 184, 152, 168, 136, 140, 172, 156, 188, 180, 148, 164, 132, 134, 166, 150, 182, 190, 158, 174, 142, 138, 170, 154, 186, 178, 146, 162, 130, 131, 163, 147, 179, 187, 155, 171, 139, 143, 175, 159, 191, 183, 151, 167, 135, 133, 165, 149, 181, 189, 157, 173, 141, 137, 169, 153, 185, 177, 145, 161, 129, 129, 161, 145, 177, 185, 153, 169, 137, 141, 173, 157, 189, 181, 149, 165, 133, 135, 167, 151, 183, 191, 159, 175, 143, 139, 171, 155, 187, 179, 147, 163, 131, 130, 162, 146, 178, 186, 154, 170, 138, 142, 174, 158, 190, 182, 150, 166, 134, 132, 164, 148, 180, 188, 156, 172, 140, 136, 168, 152, 184, 176, 144, 160, 128]
    matches = [
        {"next": next[i], "winner": None} for i in range(128)
    ]
    matches.extend(
        {"next": i // 2 + 128, "winner": None}
        for i in range(128, 255)
    )
    matches[-1]["next"] = None
    hour = 3600
    for i, match in enumerate(matches):
        if i < 192:
            match["duration"] = hour // 2
        elif i < 224:
            match["duration"] = hour
        elif i < 240:
            match["duration"] = 2 * hour
        elif i < 248:
            match["duration"] = 3 * hour
        elif i < 252:
            match["duration"] = 6 * hour
        elif i < 254:
            match["duration"] = 12 * hour
        else:
            match["duration"] = 24 * hour
    if DEBUG:
        for match in matches:
            match["duration"] = config["debug"]["match_duration"]
    return matches


def voting_command(update, context):
    if redis.get("state") != State.TAKING_SUBMISSIONS.value:
        context.bot.send_message(
            chat_id=update.effective_chat.id,
            text="The Stickerdome must be in submission phase to start voting.",
        )
        return

    if not True:
        with db:
            with db.cursor() as cur:
                cur.execute('SELECT "id" FROM "stickers" LIMIT 256')
                seed = [id for id, in cur]
                redis.set("seeding", json.dumps(seed))

    if redis.get("seeding") is None:
        context.bot.send_message(
            chat_id=update.effective_chat.id,
            text="The bracket must be manually seeded first.",
        )
        return

    sticker_chat_filter.remove_chat_ids(update.effective_chat.id)
    redis.set("state", State.VOTING.value)
    context.bot.send_message(chat_id=update.effective_chat.id, text=f"Submissions closed, it{apos}s voting time!")
    next_match()


voting_handler = CommandHandler(
    command="voting",
    callback=voting_command,
    filters=Filters.user(username=config["admins"]) & Filters.chat_type.groups,
)
dispatcher.add_handler(voting_handler)


def markdown_escape(text):
    return re.sub(r"[\\_*\[\]()~`>#+\-=|{}.!]", r"\\\g<0>", text)


def sticker_sets_command(update, context):
    lines = []
    with db:
        with db.cursor() as cur:
            cur.execute('SELECT "id", "title" FROM "sticker_sets"')
            for id_, title in cur:
                lines.append(f"[{markdown_escape(title)}](https://t.me/addstickers/{id_})")
    context.bot.send_message(
        chat_id=update.effective_chat.id,
        text="\n".join(lines),
        parse_mode=PARSEMODE_MARKDOWN_V2,
    )


sticker_sets_handler = CommandHandler(
    command="stickersets",
    callback=sticker_sets_command,
    filters=Filters.user(username=config["admins"]),
)
dispatcher.add_handler(sticker_sets_handler)


def generate_versus_image(file_id_a, file_id_b, out):
    with Image.open(project_path / "stickers" / f"{file_id_a}.webp") as img_a, Image.open(project_path / "stickers" / f"{file_id_b}.webp") as img_b:
        img = Image.open(project_path / "versus-template.png")
        img.paste(img_a, ((512 - img_a.width) // 2, (512 - img_a.height) // 2 + 100))
        img.paste(img_b, ((512 - img_b.width) // 2 + 512 + 20, (512 - img_b.height) // 2 + 100))
        img.save(out, format="PNG")


def _duration(sec):
    hours = sec // 3600
    minutes = (sec - hours * 3600) // 60
    seconds = sec - hours * 3600 - minutes * 60
    parts = []
    if hours:
        parts.append(f"{hours} hour")
    if hours > 1:
        parts[-1] += "s"
    if minutes:
        parts.append(f"{minutes} minute")
    if minutes > 1:
        parts[-1] += "s"
    if seconds:
        parts.append(f"{seconds} second")
    if seconds > 1:
        parts[-1] += "s"
    return " ".join(parts)


def new_poll(sticker_ids, match_duration):
    group_id = _int_from_bytes(redis.get("group_id"))
    if group_id is None:
        raise ValueError("Missing or invalid group_id")

    file_ids = []
    sticker_set_ids = []
    sticker_set_titles = []
    with db:
        with db.cursor() as cur:
            print("sticker ids:", sticker_ids)
            for sticker_unique_id in sticker_ids:
                cur.execute(
                    """
                    SELECT "stickers"."file_id", "stickers"."set", "sticker_sets"."title"
                    FROM "stickers" LEFT JOIN "sticker_sets"
                        ON "stickers"."set" = "sticker_sets"."id"
                    WHERE "stickers"."id" = %s
                    """,
                    (sticker_unique_id,))
                file_id, set_id, set_title = cur.fetchone()
                file_ids.append(file_id)
                sticker_set_ids.append(set_id)
                sticker_set_titles.append(set_title)

    def caption_line(index):
        emoji = [emoji_a, emoji_b][index]
        set_title = sticker_set_titles[index] or "this pack"
        if set_id := sticker_set_ids[index]:
            return fr"Sticker {emoji} is from [{markdown_escape(set_title)}](https://t.me/addstickers/{set_id})\."
        return fr"Sticker {emoji} has no pack\."

    caption = "\n".join([
        r"A new battle begins\!",
        caption_line(0),
        caption_line(1),
        fr"This poll will stay open for at least {_duration(match_duration)}\.",
    ])

    with io.BytesIO() as img:
        generate_versus_image(*file_ids, img)
        stickers_message = bot.send_photo(chat_id=group_id, photo=img.getvalue(), caption=caption, parse_mode=PARSEMODE_MARKDOWN_V2)
    poll_message = bot.send_poll(
        chat_id=group_id,
        question="Which shall win?",
        options=[emoji_a, emoji_b],
        reply_to_message_id=stickers_message.message_id
    )
    bot.pin_chat_message(chat_id=group_id, message_id=poll_message.message_id)
    redis.set("current_stickers_message", stickers_message.message_id)
    redis.set("current_poll_message", poll_message.message_id)
    redis.set("current_poll", poll_message.poll.id)
    redis.set("current_poll_start", _now())
    redis.set("current_voter_count", 0)


def current_match():
    index = _int_from_bytes(redis.get("current_match"))
    if index is None:
        return None
    try:
        matches = json.loads(redis.get("matches"))
    except TypeError:
        return None
    try:
        return matches[index]
    except IndexError:
        return None


def next_match():
    current_match_index = _int_from_bytes(redis.get("current_match"))
    print("called next_match; current_match_index is", current_match_index)

    if current_match_index is None:
        # First match
        redis.set("current_match", 0)
        matches = generate_matches()
        redis.set("matches", json.dumps(matches))
        participants = match_participants(0, matches)
        print("first round participants:", participants)
        print(matches[0])
        new_poll(participants, matches[0]["duration"])
        return

    matches = json.loads(redis.get("matches"))
    current_match = matches[current_match_index]
    current_match_participants = match_participants(current_match_index, matches)
    print("old match is", current_match_index, current_match)
    print("old participants are", current_match_participants)

    if DEBUG and current_match_index < config["debug"]["autovote_until"]:
        import random
        winner_id = random.choice(current_match_participants)
        matches[current_match_index]["winner"] = winner_id
        redis.set("matches", json.dumps(matches))
        if not config["debug"]["disable_bracket"]:
            update_bracket_image()
        redis.set("current_match", current_match_index + 1)
        next_match()
        return

    current_poll_message_id = _int_from_bytes(redis.get("current_poll_message"))

    group_id = _int_from_bytes(redis.get("group_id"))

    old_poll = None
    if current_poll_message_id is not None:
        bot.unpin_chat_message(chat_id=group_id, message_id=current_poll_message_id)
        try:
            old_poll = bot.stop_poll(chat_id=group_id, message_id=current_poll_message_id)
        except BadRequest as e:
            if e.message != "Poll has already been closed":
                raise e

    if old_poll is None:
        bot.send_message(chat_id=group_id, text="Oopsie! This requires some manual attention.")
        return
    else:
        votes_a = old_poll.options[0].voter_count
        votes_b = old_poll.options[1].voter_count
        if votes_a > votes_b:
            winner_id = current_match_participants[0]
        elif votes_a < votes_b:
            winner_id = current_match_participants[1]
        else:
            # Tiebreaker
            bot.send_message(chat_id=group_id, text="Tossing a coin to determine the winner.")
            winner_id = current_match_participants[secrets.randbelow(2)]

    with db:
        with db.cursor() as cur:
            cur.execute('SELECT "file_id" FROM "stickers" WHERE "id" = %s', (winner_id,))
            winner_file_id, = cur.fetchone()

    matches[current_match_index]["winner"] = winner_id
    matches[current_match_index]["votes"] = [votes_a, votes_b]
    redis.set("matches", json.dumps(matches))
    update_bracket_image()

    end = current_match["next"] is None

    repeat_sticker = 5 if end else 1
    for _ in range(repeat_sticker):
        bot.send_sticker(chat_id=group_id, sticker=winner_file_id)

    if end:
        redis.set("state", State.ENDED.value)
        update_match_data()
        send_bracket(
            chat_id=group_id,
            caption=r"Ohi on\! kiitos pelaamisesta vaikka äänestitte VÄÄRIN",
        )
    else:
        bot.send_message(chat_id=group_id, text="We have a winner!")
        new_match_index = current_match_index + 1
        new_match = matches[new_match_index]
        new_participants = match_participants(new_match_index, matches)
        new_poll(new_participants, new_match["duration"])
        redis.set("current_match", new_match_index)
        update_match_data()


def poll_update(update, context):
    poll = update.poll
    if poll.is_closed:
        print("this poll is closed")
        return
    current_poll_id = redis.get("current_poll")
    if current_poll_id is None:
        print("no current poll")
        return

    if poll.id != redis.get("current_poll").decode():
        print("not current poll")
        return

    redis.set("current_voter_count", poll.total_voter_count)

    if poll.total_voter_count < config["min_votes"]:
        print("not enough votes")
        return

    if poll.options[0].voter_count == poll.options[1].voter_count:
        print("it's a tie")
        return

    poll_start = _int_from_bytes(redis.get("current_poll_start"))
    if poll_start is None:
        return

    match = current_match()
    if match is None:
        return

    if _now() - poll_start < match["duration"]:
        return

    next_match()


poll_handler = PollHandler(callback=poll_update)
dispatcher.add_handler(poll_handler)


def next_command(update, context):
    if redis.get("state") != State.VOTING.value:
        return

    match_index = _int_from_bytes(redis.get("current_match"))
    if match_index is not None and match_index >= 248:
        if update.effective_user.username not in config["admins"]:
            context.bot.send_message(
                chat_id=update.effective_chat.id,
                text="Only admins can use /next at this stage.",
            )
            return

    match = current_match()
    if match is None:
        return

    poll_start = _int_from_bytes(redis.get("current_poll_start"))
    if poll_start is not None:
        if _now() - poll_start < match["duration"]:
            poll_end = poll_start + match["duration"]
            context.bot.send_message(
                chat_id=update.effective_chat.id,
                text=f"This poll can be closed in {_duration(poll_end - _now())}."
            )
            return

    voter_count = _int_from_bytes(redis.get("current_voter_count"))
    if voter_count is not None and voter_count < config["min_votes"]:
        context.bot.send_message(
            chat_id=update.effective_chat.id,
            text="Not enough votes to change poll."
        )
        return

    next_match()


next_handler = CommandHandler(
    command="next",
    callback=next_command,
    filters=Filters.chat_type.groups
)
dispatcher.add_handler(next_handler)

end_handler = CommandHandler(
    command="end",
    callback=next_command,
    filters=Filters.chat_type.groups & Filters.user(username=config["admins"])
)
dispatcher.add_handler(end_handler)


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
    if state == b'reset':
        reset()
    else:
        raise ValueError("Invalid state in Redis")

if state == State.NOT_STARTED.value:
    if group_id_bytes is not None:
        raise ValueError("group_id should not exist in Redis")

if state in [State.TAKING_SUBMISSIONS.value, State.VOTING.value, State.ENDED.value]:
    if group_id is None:
        raise ValueError("Missing or invalid group_id in Redis")

if state == State.TAKING_SUBMISSIONS.value:
    for key in [
        "current_match",
        "current_stickers_message",
        "current_poll_message",
        "current_poll",
        "current_poll_start",
        "current_voter_count",
        "matches",
    ]:
        if redis.get(key) is not None:
            raise ValueError(f"{key} should not exist in Redis")
    sticker_chat_filter.add_chat_ids(group_id)

if state == State.VOTING.value:
    for key in [
        "current_match",
        #"current_stickers_message",
        #"current_poll_message",
        #"current_poll",
        #"current_poll_start",
    ]:
        if _int_from_bytes(redis.get(key)) is None:
            raise ValueError(f"Missing or invalid {key} in Redis")
    for key in [
        "matches",
        "seeding",
    ]:
        if redis.get(key) is None:
            raise ValueError(f"Missing {key} in Redis")

update_match_data()

if (
    config["downtime_notifications"]
    and (group_id := _int_from_bytes(redis.get("group_id"))) is not None
):
    bot.send_message(chat_id=group_id, text="The Stickerdome is back up! Sorry for the downtime.")

updater.start_webhook(
    listen="127.0.0.1",
    port=config["webhook_port"],
    webhook_url=config["webhook_url"],
)
