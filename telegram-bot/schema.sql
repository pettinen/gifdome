CREATE TABLE IF NOT EXISTS "stickers" (
  "id" text PRIMARY KEY,
  "file_id" text NOT NULL,
  "set" text,
  "width" smallint NOT NULL,
  "height" smallint NOT NULL,
  "submitter" integer REFERENCES "users"("id")
);

CREATE TABLE IF NOT EXISTS "users" (
  "id" integer PRIMARY KEY,
  "username" text NOT NULL
);

CREATE TABLE IF NOT EXISTS "submissions" (
  "user_id" integer NOT NULL REFERENCES "users"("id"),
  "sticker_id" text NOT NULL REFERENCES "stickers"("id"),
  PRIMARY KEY ("user_id", "sticker_id")
);
