CREATE TYPE "chat_type" AS ENUM('group', 'supergroup');
CREATE TYPE "matchup_state" AS ENUM('not_started', 'started', 'finished', 'aborted');
CREATE TYPE "tournament_state" AS ENUM('submitting', 'voting', 'finished', 'aborted');

CREATE TABLE "chats" (
    "id" bigint PRIMARY KEY,
    "type" chat_type NOT NULL,
    "title" text NOT NULL
);

CREATE TABLE "duplicate_submissions" (
    "animation_a_id" text REFERENCES "animations"("id"),
    "animation_b_id" text REFERENCES "animations"("id"),
    PRIMARY KEY ("animation_a_id", "animation_b_id"),
    CHECK ("animation_a_id" != "animation_b_id")
);

CREATE TABLE "duplicates" (
    "primary_animation_id" text REFERENCES "animations"("id"),
    "duplicate_animation_id" text REFERENCES "animations"("id"),
    PRIMARY KEY ("primary_animation_id", "duplicate_animation_id"),
    CHECK ("primary_animation_id" != "duplicate_animation_id")
);

CREATE TABLE "animations" (
    "id" text PRIMARY KEY,
    "file_identifier" text NOT NULL,
    "width" integer NOT NULL,
    "height" integer NOT NULL,
    "mime_type" text NOT NULL,
    "frames" integer NOT NULL,
    "fps_num" integer NOT NULL,
    "fps_denom" integer NOT NULL,
    "description" text CHECK ("description" != '')
);

CREATE TABLE "animation_filenames" (
    "animation_id" text REFERENCES "animations"("id"),
    "filename" text,
    PRIMARY KEY ("animation_id", "filename")
);

CREATE TABLE "matchups" (
    "tournament_id" text REFERENCES "tournaments"("id"),
    "index" integer CHECK ("index" >= 0),
    "poll_id" text,
    "message_id" integer,
    "animation_a_id" text NOT NULL REFERENCES "animations"("id"),
    "animation_b_id" text NOT NULL REFERENCES "animations"("id"),
    "state" matchup_state NOT NULL,
    "animation_a_votes" integer CHECK ("animation_a_votes" >= 0),
    "animation_b_votes" integer CHECK ("animation_b_votes" >= 0),
    "duration_secs" integer NOT NULL,
    "started_at" timestamp (6) with time zone,
    "finished_at" timestamp (6) with time zone,
    PRIMARY KEY ("tournament_id", "index"),
    CHECK ("animation_a_id" != "animation_b_id"),
    CHECK (
        (
            "state" = 'not_started' AND
            "poll_id" IS NULL AND
            "message_id" IS NULL AND
            "animation_a_votes" IS NULL AND
            "animation_b_votes" IS NULL AND
            "started_at" IS NULL AND
            "finished_at" IS NULL
        ) OR
        (
            "state" = 'started' AND
            "poll_id" IS NOT NULL AND
            "message_id" IS NOT NULL AND
            "animation_a_votes" IS NOT NULL AND
            "animation_b_votes" IS NOT NULL AND
            "started_at" IS NOT NULL AND
            "finished_at" IS NULL
        ) OR
        (
            "state" = 'finished' AND
            "poll_id" IS NOT NULL AND
            "message_id" IS NOT NULL AND
            "animation_a_votes" IS NOT NULL AND
            "animation_b_votes" IS NOT NULL AND
            "started_at" IS NOT NULL AND
            "finished_at" IS NOT NULL
        )
    )
);

CREATE UNIQUE INDEX ON "matchups"("tournament_id", "index")
    WHERE "state" = 'started';

CREATE TABLE "submissions" (
    "tournament_id" text REFERENCES "tournaments"("id"),
    "animation_id" text REFERENCES "animations"("id"),
    "submitter_id" bigint REFERENCES "users"("id"),
    "created_at" timestamp (6) with time zone NOT NULL,
    PRIMARY KEY ("tournament_id", "animation_id", "submitter_id")
);

CREATE TABLE "tournaments" (
    "id" text PRIMARY KEY,
    "chat_id" bigint NOT NULL REFERENCES "chats"("id"),
    "state" tournament_state NOT NULL,
    "created_at" timestamp (6) with time zone NOT NULL,
);

CREATE UNIQUE INDEX ON "tournaments"("chat_id")
    WHERE "state" IN ('submitting', 'voting');

CREATE TABLE "users" (
    "id" bigint PRIMARY KEY,
    "username" text NOT NULL
);
