use chrono::{DateTime, Duration, Utc};
use frankenstein::{AsyncTelegramApi, StopPollParams};

use crate::{tournament::advance_matchup, API, DB};

pub async fn run_scheduled_task() {
    let mut db = DB.wait().lock().await;
    let t = match db.transaction().await {
        Ok(t) => t,
        Err(err) => {
            eprintln!("failed to start transaction in scheduled task: {err}");
            return;
        }
    };

    let rows = match t
        .query(
            r#"
            SELECT
                "matchups"."tournament_id",
                "matchups"."index",
                "matchups"."message_id",
                "matchups"."duration_secs",
                "matchups"."started_at",
                "matchups"."animation_a_votes",
                "matchups"."animation_b_votes",
                "tournaments"."chat_id",
                "tournaments"."min_votes"
            FROM "matchups"
                JOIN "tournaments" ON "matchups"."tournament_id" = "tournaments"."id"
            WHERE "matchups"."state" = 'started'
            "#,
            &[],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("failed to query matchups in scheduled task: {err}");
            return;
        }
    };

    let now = Utc::now();
    for row in rows {
        let message_id = match row.get::<_, Option<i32>>("message_id") {
            Some(message_id) => message_id,
            None => {
                eprintln!("db integrity error: missing message_id from started matchup");
                continue;
            }
        };
        let started_at = match row.get::<_, Option<DateTime<Utc>>>("started_at") {
            Some(started_at) => started_at,
            None => {
                eprintln!("db integrity error: missing started_at from started matchup");
                continue;
            }
        };
        let expires = started_at + Duration::seconds(row.get::<_, i32>("duration_secs").into());
        let votes_a = match row.get::<_, Option<i32>>("animation_a_votes") {
            Some(votes) => votes,
            None => {
                eprintln!("db integrity error: missing animation_a_votes from started matchup");
                continue;
            }
        };
        let votes_b = match row.get::<_, Option<i32>>("animation_b_votes") {
            Some(votes) => votes,
            None => {
                eprintln!("db integrity error: missing animation_b_votes from started matchup");
                continue;
            }
        };
        let min_votes = match row.get::<_, Option<i16>>("min_votes") {
            Some(min_votes) => min_votes,
            None => {
                eprintln!("db integrity error: missing min_votes from started tournament");
                continue;
            }
        };

        if expires < now && votes_a != votes_b && votes_a + votes_b >= min_votes.into() {
            let count = match t
                .execute(
                    r#"
                    UPDATE "matchups" SET "state" = 'finished', "finished_at" = $1
                    WHERE "message_id" = $2 AND "state" = 'started'
                    "#,
                    &[&now, &message_id],
                )
                .await
            {
                Ok(count) => count,
                Err(err) => {
                    eprintln!("failed to update matchup in scheduled task: {err}");
                    continue;
                }
            };
            if count != 1 {
                eprintln!(
                    "db integrity error: expected to update 1 matchup, but updated {count} rows"
                );
                continue;
            }

            let api = API.wait();
            if let Err(err) = api
                .stop_poll(
                    &StopPollParams::builder()
                        .chat_id(row.get::<_, i64>("chat_id"))
                        .message_id(message_id)
                        .build(),
                )
                .await
            {
                eprintln!("failed to stop poll in scheduled task: {err}");
                continue;
            }

            if let Err(err) = advance_matchup(&t, row.get("tournament_id"), row.get("index")).await
            {
                eprintln!("failed to advance matchup: {err}");
                continue;
            }
        }
    }
    if let Err(err) = t.commit().await {
        eprintln!("failed to commit transaction in scheduled task: {err}");
    }
}
