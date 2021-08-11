CREATE TABLE IF NOT EXISTS "users" (
  "id" integer PRIMARY KEY,
  "username" text NOT NULL
);

CREATE TABLE IF NOT EXISTS "gifs" (
  "id" text PRIMARY KEY,
  "file_id" text NOT NULL,
  "file_size" integer,
  "mime_type" text,
  "width" smallint NOT NULL,
  "height" smallint NOT NULL,
  "duration" smallint NOT NULL,
  "submitter" integer REFERENCES "users"("id")
);

CREATE TABLE IF NOT EXISTS "gif_filenames" (
  "gif_id" text REFERENCES "gifs"("id"),
  "filename" text,
  PRIMARY KEY ("gif_id", "filename")
);

CREATE TABLE IF NOT EXISTS "submissions" (
  "user_id" integer REFERENCES "users"("id"),
  "gif_id" text REFERENCES "gifs"("id"),
  "created" timestamp with time zone NOT NULL,
  PRIMARY KEY ("user_id", "gif_id")
);
