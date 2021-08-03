import psycopg2
from flask import Flask
from redis import Redis


def create_app(*args):
    from .api_v1 import api_v1

    app = Flask(__name__)
    app.register_blueprint(api_v1, url_prefix="/api/v1")

    return app


db = psycopg2.connect()
redis = Redis(unix_socket_path="/run/redis/redis.sock", db=15)
