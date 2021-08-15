import json

from flask import Blueprint, jsonify

from . import db, redis


api_v1 = Blueprint("api_v1", __name__)


@api_v1.get("/matches.json")
def matches():
    raw = redis.get("matches")
    if raw is None:
        return jsonify([])
    return jsonify(json.loads(raw))


@api_v1.get("/gifs.json")
def gifs():
    with db:
        with db.cursor() as cur:
            cur.execute(
                """
                SELECT "id", "file_id", "file_size", "mime_type", "width", "height", "duration"
                FROM "gifs"
                """)
            data = {}
            for id_, file_id, file_size, mime_type, width, height, duration in cur:
                data[id_] = {
                    "id": id_,
                    "file": file_id,
                    "filenames": [],
                    "mime_type": mime_type,
                    "width": width,
                    "height": height,
                    "duration": duration,
                }
            cur.execute('SELECT "gif_id", "filename" FROM "gif_filenames"')
            for gif_id, filename in cur:
                data[gif_id]["filenames"].append(filename)
            return data


@api_v1.get("/submissions.json")
def submissions():
    with db:
        with db.cursor() as cur:
            cur.execute(
                'SELECT "gif_id", count(*) FROM "submissions" GROUP BY "gif_id"'
            )
            return {id_: count for id_, count in cur}
