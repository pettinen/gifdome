import json

from flask import Blueprint, jsonify

from . import db, redis


api_v1 = Blueprint("api_v1", __name__)


@api_v1.get("/matches.json")
def matches():
    seeding = json.loads(redis.get("seeding"))
    matches = json.loads(redis.get("matches"))

    data = []
    for i, match in enumerate(matches):
        if i < 128:
            match["participants"] = seeding[2 * i : 2 * i + 2]
        else:
            match["participants"] = [m["winner"] for m in matches if m["next"] == i]
    return jsonify(matches)


@api_v1.get("/stickers.json")
def stickers():
    with db:
        with db.cursor() as cur:
            cur.execute(
                """
                SELECT
                    "stickers"."id",
                    "stickers"."file_id",
                    "stickers"."width",
                    "stickers"."height",
                    "sticker_sets"."id",
                    "sticker_sets"."title"
                FROM "stickers"
                    LEFT JOIN "sticker_sets" ON "stickers"."set" = "sticker_sets"."id"
                """)
            data = {}
            for id_, file_id, width, height, set_id, set_title in cur:
                sticker = {
                    "id": id_,
                    "file": file_id,
                    "width": width,
                    "height": height,
                }
                if set_id is not None:
                    sticker["set"] = {
                        "id": set_id,
                        "title": set_title,
                    }
                data[id_] = sticker
            return data


@api_v1.get("/submissions.json")
def submissions():
    data = {}
    with db:
        with db.cursor() as cur:
            cur.execute(
                'SELECT "sticker_id", count(*) FROM "submissions" GROUP BY "sticker_id"'
            )
            return {id_: count for id_, count in cur}
