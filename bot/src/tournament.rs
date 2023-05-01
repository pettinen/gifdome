use std::{cmp::Ordering, collections::HashMap, time::Duration};

use chrono::Utc;
use deadpool_postgres::Transaction;
use frankenstein::{
    api_params::File as ApiFileParam, AsyncTelegramApi, InputFile, PinChatMessageParams,
    SendAnimationParams, SendPollParams,
};
use rand::{seq::SliceRandom, thread_rng};
use time_humanize::{Accuracy, HumanTime, Tense};

use crate::{
    animation::{self, combine_animations},
    util::update_chat_commands,
    API, CONFIG,
};

#[derive(Debug, thiserror::Error)]
pub enum AnnounceMatchupWinnerError {
    #[error("API error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("matchup votes are equal")]
    EqualVotes,
}

#[derive(Debug, thiserror::Error)]
pub enum SendPollError {
    #[error("failed to combine animations: {0}")]
    CombineAnimationsError(#[from] animation::CombineAnimationsError),
    #[error("failed to convert matchup duration: {0}")]
    InvalidDurationError(#[from] std::num::TryFromIntError),
    #[error("missing animation id")]
    MissingAnimationId,
    #[error("poll missing from sent message")]
    MissingPoll,
    #[error("failed to query matchup: {0}")]
    QueryMatchupError(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to send animation: {0}")]
    SendAnimationFailed(#[source] frankenstein::Error),
    #[error("failed to send poll: {0}")]
    SendPollFailed(#[source] frankenstein::Error),
}

pub async fn send_poll(
    t: &Transaction<'_>,
    chat_id: i64,
    tournament_id: &str,
    new_matchup_index: i32,
) -> Result<(String, i32), SendPollError> {
    let matchup = t
        .query_one(
            r#"
            SELECT "round", "animation_a_id", "animation_b_id", "duration_secs"
            FROM "matchups"
            WHERE "tournament_id" = $1 AND "index" = $2
            "#,
            &[&tournament_id, &new_matchup_index],
        )
        .await
        .map_err(SendPollError::QueryMatchupError)?;

    let animation_a_id = matchup
        .get::<_, Option<String>>("animation_a_id")
        .ok_or(SendPollError::MissingAnimationId)?;
    let animation_b_id = matchup
        .get::<_, Option<String>>("animation_b_id")
        .ok_or(SendPollError::MissingAnimationId)?;

    let api = API.wait();
    let combined_file_path = combine_animations(&animation_a_id, &animation_b_id).await?;

    let duration_secs = matchup.get::<_, i32>("duration_secs").try_into()?;
    let round: u32 = matchup.get::<_, i16>("round").try_into()?;
    let round_str = match round {
        1 => "This is the final round!".to_string(),
        2 => "We\u{2019}re in the semifinals.".to_string(),
        3 => "We\u{2019}re in the quarterfinals.".to_string(),
        _ => format!(
            "We\u{2019}re in the round of {matchups_in_round}.",
            matchups_in_round = 2i32.pow(round),
        ),
    };
    let animation_message = match api
        .send_animation(
            &SendAnimationParams::builder()
                .chat_id(chat_id)
                .animation(ApiFileParam::InputFile(
                    InputFile::builder()
                        .path(combined_file_path.clone())
                        .build(),
                ))
                .caption(format!(
                    "Match #{index} begins! {round_str}\n\n\
                    This poll stays open for at least {duration}.",
                    index = new_matchup_index + 1,
                    duration = HumanTime::from(Duration::from_secs(duration_secs))
                        .to_text_en(Accuracy::Precise, Tense::Present),
                ))
                .build(),
        )
        .await
    {
        Ok(response) => response.result,
        Err(err) => {
            if let Err(err) = std::fs::remove_file(&combined_file_path) {
                eprintln!("failed to remove temp animation: {err}");
            }
            return Err(SendPollError::SendAnimationFailed(err));
        }
    };

    if let Err(err) = std::fs::remove_file(&combined_file_path) {
        eprintln!("failed to remove temp animation: {err}");
    }

    let config = CONFIG.wait();
    let poll_message = api
        .send_poll(
            &SendPollParams::builder()
                .chat_id(chat_id)
                .question("Cast your votes!")
                .options(vec![
                    config.poll.option_a_text.clone(),
                    config.poll.option_b_text.clone(),
                ])
                .reply_to_message_id(animation_message.message_id)
                .build(),
        )
        .await
        .map_err(SendPollError::SendPollFailed)?
        .result;

    if let Err(err) = api
        .pin_chat_message(
            &PinChatMessageParams::builder()
                .chat_id(chat_id)
                .message_id(poll_message.message_id)
                .disable_notification(true)
                .build(),
        )
        .await
    {
        eprintln!("failed to pin message: {err}");
    }

    let poll_id = match poll_message.poll {
        Some(poll) => poll.id,
        None => return Err(SendPollError::MissingPoll),
    };

    Ok((poll_id, poll_message.message_id))
}

pub async fn announce_matchup_winner(
    t: &Transaction<'_>,
    matchup_index: i32,
    chat_id: i64,
    animation_a_id: &str,
    animation_b_id: &str,
    votes_a: u32,
    votes_b: u32,
) -> Result<(), AnnounceMatchupWinnerError> {
    if votes_a == votes_b {
        return Err(AnnounceMatchupWinnerError::EqualVotes);
    }
    let config = CONFIG.wait();
    let (animation_id, option_text) = if votes_a > votes_b {
        (animation_a_id, &config.poll.option_a_text)
    } else {
        (animation_b_id, &config.poll.option_b_text)
    };

    t.execute("SELECT NULL", &[]).await.ok();
    let animation_file_id = t
        .query_one(
            r#"SELECT "file_identifier" FROM "animations" WHERE "id" = $1"#,
            &[&animation_id],
        )
        .await?
        .get("file_identifier");

    let api = API.wait();
    api.send_animation(
        &SendAnimationParams::builder()
            .chat_id(chat_id)
            .animation(ApiFileParam::String(animation_file_id))
            .caption(format!(
                "GIF {option_text} wins match #{match_number}!",
                match_number = matchup_index + 1,
            ))
            .build(),
    )
    .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum AdvanceMatchupError {
    #[error("failed to announce matchup winner: {0}")]
    AnnounceMatchupWinnerError(#[from] AnnounceMatchupWinnerError),
    #[error("failed to calculate matchups for new round: {0}")]
    CalculateNewRoundMatchupsError(#[from] CalculateNewRoundMatchupsError),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("matchup votes are equal")]
    EqualVotes,
    #[error("could not convert vote counts: {0}")]
    InvalidVotes(#[from] std::num::TryFromIntError),
    #[error("failed to finish tournament: {0}")]
    FinishTournamentError(#[from] FinishTournamentError),
    #[error("could not find matchup by index")]
    MatchupNotFound,
    #[error("failed to send poll: {0}")]
    SendPollError(#[from] SendPollError),
}

pub async fn advance_matchup(
    t: &Transaction<'_>,
    tournament_id: &str,
    ended_matchup_index: i32,
) -> Result<(), AdvanceMatchupError> {
    let new_matchup_index = ended_matchup_index + 1;
    let rows = t
        .query(
            r#"
            SELECT
                "tournaments"."chat_id",
                "tournaments"."rounds",
                "matchups"."index",
                "matchups"."round",
                "matchups"."animation_a_id",
                "matchups"."animation_b_id",
                "matchups"."animation_a_votes",
                "matchups"."animation_b_votes"
            FROM "matchups"
                JOIN "tournaments" ON "matchups"."tournament_id" = "tournaments"."id"
            WHERE "matchups"."tournament_id" = $1 AND "matchups"."index" IN ($2, $3)
            "#,
            &[&tournament_id, &ended_matchup_index, &new_matchup_index],
        )
        .await?;

    let mut ended_matchup = None;
    let mut new_matchup = None;
    for row in rows {
        let index = row.get::<_, i32>("index");
        if index == ended_matchup_index {
            if ended_matchup.is_some() {
                return Err(AdvanceMatchupError::DbIntegrityError(format!(
                    "multiple matchups with index {index}"
                )));
            }
            ended_matchup = Some(row);
        } else if index == new_matchup_index {
            if new_matchup.is_some() {
                return Err(AdvanceMatchupError::DbIntegrityError(format!(
                    "multiple matchups with index {index}"
                )));
            }
            new_matchup = Some(row);
        } else {
            return Err(AdvanceMatchupError::DbIntegrityError(
                "unexpected matchup index".to_string(),
            ));
        }
    }
    let ended_matchup = ended_matchup.ok_or(AdvanceMatchupError::MatchupNotFound)?;
    let ended_matchup_round = ended_matchup.get::<_, i16>("round");
    let chat_id = ended_matchup.get("chat_id");
    let rounds = match ended_matchup.get::<_, Option<i16>>("rounds") {
        Some(rounds) => rounds,
        None => {
            return Err(AdvanceMatchupError::DbIntegrityError(
                "tournament has no rounds".to_string(),
            ))
        }
    };

    let votes_a: i32 = ended_matchup.get("animation_a_votes");
    let votes_b: i32 = ended_matchup.get("animation_b_votes");
    if votes_a == votes_b {
        return Err(AdvanceMatchupError::EqualVotes);
    }
    let new_matchup = match new_matchup {
        Some(new_matchup) => new_matchup,
        None => {
            return Ok(finish_tournament(&t, tournament_id, chat_id, ended_matchup_index).await?)
        }
    };
    let new_matchup_round = new_matchup.get::<_, i16>("round");

    match ended_matchup_round.cmp(&new_matchup_round) {
        Ordering::Greater => {
            calculate_new_round_matchups(&t, tournament_id, rounds, new_matchup_round).await?;
        }
        Ordering::Equal => {}
        Ordering::Less => {
            return Err(AdvanceMatchupError::DbIntegrityError(
                "ended matchup round is less than new matchup round".to_string(),
            ))
        }
    }

    announce_matchup_winner(
        t,
        ended_matchup_index,
        ended_matchup.get("chat_id"),
        ended_matchup.get("animation_a_id"),
        ended_matchup.get("animation_b_id"),
        votes_a.try_into()?,
        votes_b.try_into()?,
    )
    .await?;

    let (poll_id, message_id) = send_poll(&t, chat_id, tournament_id, new_matchup_index).await?;

    let count = t
        .execute(
            r#"
            UPDATE "matchups" SET
                "message_id" = $1,
                "poll_id" = $2,
                "state" = 'started',
                "animation_a_votes" = 0,
                "animation_b_votes" = 0,
                "started_at" = $3
            WHERE "tournament_id" = $4 AND "index" = $5
            "#,
            &[
                &message_id,
                &poll_id,
                &Utc::now(),
                &tournament_id,
                &new_matchup_index,
            ],
        )
        .await?;
    if count != 1 {
        return Err(AdvanceMatchupError::DbIntegrityError(format!(
            "{count} rows updated"
        )));
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum FinishTournamentError {
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("votes are equal")]
    EqualVotes,
    #[error("missing animation ID")]
    MissingAnimationId,
    #[error("missing votes")]
    MissingVotes,
    #[error("failed to query winning animation: {0}")]
    QueryAnimationFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to query final matchup: {0}")]
    QueryMatchupFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to send animation: {0}")]
    SendAnimationFailed(#[source] frankenstein::Error),
    #[error("failed to update tournament status to finished: {0}")]
    UpdateTournamentFailed(#[source] deadpool_postgres::tokio_postgres::Error),
}

pub async fn finish_tournament(
    t: &Transaction<'_>,
    tournament_id: &str,
    chat_id: i64,
    ended_matchup_index: i32,
) -> Result<(), FinishTournamentError> {
    let count = t
        .execute(
            r#"UPDATE "tournaments" SET "state" = 'finished' WHERE "id" = $1"#,
            &[&tournament_id],
        )
        .await
        .map_err(FinishTournamentError::UpdateTournamentFailed)?;
    if count != 1 {
        return Err(FinishTournamentError::DbIntegrityError(format!(
            "expected to update one tournament, updated {count} rows"
        )));
    }

    let matchup = t
        .query_one(
            r#"
            SELECT
                "animation_a_id",
                "animation_b_id",
                "animation_a_votes",
                "animation_b_votes"
            FROM "matchups"
            WHERE "tournament_id" = $1 AND "index" = $2
            "#,
            &[&tournament_id, &ended_matchup_index],
        )
        .await
        .map_err(FinishTournamentError::QueryMatchupFailed)?;

    let votes_a = match matchup.get::<_, Option<i32>>("animation_a_votes") {
        Some(votes) => votes,
        None => return Err(FinishTournamentError::MissingVotes),
    };
    let votes_b = match matchup.get::<_, Option<i32>>("animation_b_votes") {
        Some(votes) => votes,
        None => return Err(FinishTournamentError::MissingVotes),
    };
    let winner_id = match votes_a.cmp(&votes_b) {
        Ordering::Less => match matchup.get::<_, Option<String>>("animation_b_id") {
            Some(id) => id,
            None => return Err(FinishTournamentError::MissingAnimationId),
        },
        Ordering::Equal => return Err(FinishTournamentError::EqualVotes),
        Ordering::Greater => match matchup.get::<_, Option<String>>("animation_a_id") {
            Some(id) => id,
            None => return Err(FinishTournamentError::MissingAnimationId),
        },
    };

    let file_id = t
        .query_one(
            r#"SELECT "file_identifier" FROM "animations" WHERE "id" = $1"#,
            &[&winner_id],
        )
        .await
        .map_err(FinishTournamentError::QueryAnimationFailed)?
        .get("file_identifier");

    let api = API.wait();
    let message = api
        .send_animation(
            &SendAnimationParams::builder()
                .chat_id(chat_id)
                .animation(ApiFileParam::String(file_id))
                .caption("This is, officially, the best GIF. Thanks for voting!")
                .build(),
        )
        .await
        .map_err(FinishTournamentError::SendAnimationFailed)?
        .result;

    if let Err(err) = api
        .pin_chat_message(
            &PinChatMessageParams::builder()
                .chat_id(chat_id)
                .message_id(message.message_id)
                .disable_notification(true)
                .build(),
        )
        .await
    {
        eprintln!("failed to pin message: {err}");
    }

    if let Err(err) = update_chat_commands(chat_id, None).await {
        eprintln!("failed to update chat commands: {err}");
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum GenerateSeedsError {
    #[error("failed to convert previous round size to u32: {0}")]
    ConvertError(#[from] std::num::TryFromIntError),
}

fn generate_seeds(rounds: u32) -> Result<Vec<u32>, GenerateSeedsError> {
    fn next_seeds(previous: &[u32]) -> Result<Vec<u32>, GenerateSeedsError> {
        let new_len = previous.len() * 2;
        let new_len_u32: u32 = new_len.try_into()?;
        let mut next = Vec::with_capacity(new_len);
        for seed in previous {
            next.push(*seed);
            next.push(new_len_u32 - *seed - 1);
        }
        Ok(next)
    }

    let mut seeds = vec![0, 1];
    for _ in 2..=rounds {
        seeds = next_seeds(&seeds)?;
    }
    Ok(seeds)
}

#[derive(Debug, thiserror::Error)]
pub enum CreateBracketError {
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("failed to insert matchup: {0}")]
    InsertMatchupFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("could not convert integer")]
    ConvertError(#[from] std::num::TryFromIntError),
    #[error("not enough submissions ({0}, need at least {1}")]
    NotEnoughSubmissions(usize, u32),
    #[error("failed to query submissions: {0}")]
    QuerySubmissionsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("unexpected error: out-of-bounds Vec access")]
    UnexpectedIndex,
    #[error("unexpected error: missing HashMap key")]
    UnexpectedMissingHashMapKey,
}

pub async fn create_bracket(
    t: &Transaction<'_>,
    tournament_id: &str,
    rounds: u32,
) -> Result<(), CreateBracketError> {
    let submissions = t
        .query(
            r#"
            SELECT
                COALESCE(
                    (
                        SELECT "duplicates"."primary_animation_id" FROM "duplicates"
                        WHERE "duplicates"."duplicate_animation_id" = "submissions"."animation_id"
                    ),
                    "submissions"."animation_id"
                ) AS "unique_animation_id",
                count(DISTINCT "submitter_id") AS "count"
            FROM "submissions"
            WHERE "tournament_id" = $1
            GROUP BY "unique_animation_id"
            ORDER BY "count" DESC
            "#,
            &[&tournament_id],
        )
        .await
        .map_err(CreateBracketError::QuerySubmissionsFailed)?;

    let submission_count = submissions.len();
    let min_submissions = 2usize.pow(rounds);

    if submission_count < min_submissions {
        return Err(CreateBracketError::NotEnoughSubmissions(
            submission_count,
            min_submissions.try_into()?,
        ));
    }

    let mut submissions_by_count = HashMap::new();
    for submission in submissions {
        let count: i64 = submission.get("count");
        submissions_by_count
            .entry(count)
            .or_insert_with(Vec::new)
            .push(submission.get::<_, String>("unique_animation_id"));
    }

    {
        let mut rng = thread_rng();
        for (_, submissions) in submissions_by_count.iter_mut() {
            submissions.shuffle(&mut rng);
        }
    }

    let mut counts = submissions_by_count.keys().collect::<Vec<_>>();
    counts.sort_by(|a, b| b.cmp(a));

    struct Matchup<'a> {
        index: i16,
        round: u32,
        animation_a_id: Option<&'a String>,
        animation_b_id: Option<&'a String>,
        duration_secs: u16,
    }

    let mut remaining_submissions = min_submissions;
    let mut sorted_submissions = Vec::<&String>::new();

    for count in &counts {
        let submissions = match submissions_by_count.get(count) {
            Some(submissions) => submissions,
            None => return Err(CreateBracketError::UnexpectedMissingHashMapKey),
        };
        if remaining_submissions >= submissions.len() {
            sorted_submissions.extend(submissions.iter());
            remaining_submissions -= submissions.len()
        } else {
            sorted_submissions.extend(submissions.iter().take(remaining_submissions));
            break;
        }
    }

    let config = CONFIG.wait();
    let mut matchups = Vec::with_capacity(min_submissions - 1);
    let seeds = match generate_seeds(rounds) {
        Ok(seeds) => seeds,
        Err(GenerateSeedsError::ConvertError(err)) => return Err(err.into()),
    };

    let mut index = 0;
    for i in 0..min_submissions / 2 {
        let seed_index1 = seeds
            .get(i * 2)
            .ok_or(CreateBracketError::UnexpectedIndex)?;
        let seed_index1: usize = (*seed_index1).try_into()?;

        let seed_index2 = seeds
            .get(i * 2 + 1)
            .ok_or(CreateBracketError::UnexpectedIndex)?;
        let seed_index2: usize = (*seed_index2).try_into()?;

        matchups.push(Matchup {
            index,
            round: rounds,
            animation_a_id: Some(
                sorted_submissions
                    .get(seed_index1)
                    .ok_or(CreateBracketError::UnexpectedIndex)?,
            ),
            animation_b_id: Some(
                sorted_submissions
                    .get(seed_index2)
                    .ok_or(CreateBracketError::UnexpectedIndex)?,
            ),
            duration_secs: *config
                .tournament
                .round_lengths_secs
                .get(rounds as usize - 1)
                .ok_or(CreateBracketError::UnexpectedIndex)?,
        });
        index += 1;
    }

    for round in (1..rounds).rev() {
        let matchup_count = 2u32.pow(round - 1);

        for _ in 0..matchup_count {
            matchups.push(Matchup {
                index,
                round,
                animation_a_id: None,
                animation_b_id: None,
                duration_secs: *config
                    .tournament
                    .round_lengths_secs
                    .get(round as usize - 1)
                    .ok_or(CreateBracketError::UnexpectedIndex)?,
            });
            index += 1;
        }
    }

    for matchup in matchups {
        let count = t
            .execute(
                r#"
                INSERT INTO "matchups" (
                    "tournament_id",
                    "index",
                    "round",
                    "animation_a_id",
                    "animation_b_id",
                    "state",
                    "duration_secs"
                ) VALUES ($1, $2, $3, $4, $5, 'not_started', $6)
                "#,
                &[
                    &tournament_id,
                    &i32::from(matchup.index),
                    &i16::try_from(matchup.round)?,
                    &matchup.animation_a_id,
                    &matchup.animation_b_id,
                    &i32::from(matchup.duration_secs),
                ],
            )
            .await
            .map_err(CreateBracketError::InsertMatchupFailed)?;
        if count != 1 {
            return Err(CreateBracketError::DbIntegrityError(format!(
                "expected to insert one matchup, inserted {count} rows"
            )));
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CalculateNewRoundMatchupsError {
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("invalid round number: {0}")]
    InvalidTotalRounds(#[from] std::num::TryFromIntError),
    #[error("missing matchup (index {0}")]
    MissingMatchup(u32),
    #[error("failed to query matchups: {0}")]
    QueryMatchupFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to update matchup: {0}")]
    UpdateMatchupFailed(#[source] deadpool_postgres::tokio_postgres::Error),
}

async fn calculate_new_round_matchups(
    t: &Transaction<'_>,
    tournament_id: &str,
    total_rounds: i16,
    round_number: i16,
) -> Result<(), CalculateNewRoundMatchupsError> {
    let total_rounds: u32 = total_rounds.try_into()?;
    let round_number: u32 = round_number.try_into()?;
    let start_index: u32 = (round_number..total_rounds).map(|r| 2u32.pow(r)).sum();
    let end_index = start_index + 2u32.pow(round_number - 1);

    let previous_round_end_inclusive = start_index - 1;
    let previous_round_start = start_index - 2u32.pow(round_number);

    let mut x = 2u32.pow(round_number);

    let matchup_rows = t
        .query(
            r#"
            SELECT
                "index",
                "animation_a_id",
                "animation_b_id",
                "animation_a_votes",
                "animation_b_votes"
            FROM "matchups"
            WHERE "tournament_id" = $1 AND "index" BETWEEN $2 AND $3
            "#,
            &[
                &tournament_id,
                &i32::try_from(previous_round_start)?,
                &(i32::try_from(previous_round_end_inclusive)?),
            ],
        )
        .await
        .map_err(CalculateNewRoundMatchupsError::QueryMatchupFailed)?;

    struct Matchup {
        animation_a_id: String,
        animation_b_id: String,
        animation_a_votes: i32,
        animation_b_votes: i32,
    }

    let mut matchups = HashMap::with_capacity(matchup_rows.len());

    for row in matchup_rows {
        let animation_a_id: String = row.get::<_, Option<String>>("animation_a_id").ok_or(
            CalculateNewRoundMatchupsError::DbIntegrityError(
                "matchup has no animation A".to_owned(),
            ),
        )?;
        let animation_b_id: String = row.get::<_, Option<String>>("animation_b_id").ok_or(
            CalculateNewRoundMatchupsError::DbIntegrityError(
                "matchup has no animation B".to_owned(),
            ),
        )?;
        let animation_a_votes = row.get::<_, Option<i32>>("animation_a_votes").ok_or(
            CalculateNewRoundMatchupsError::DbIntegrityError(
                "matchup has no animation A votes".to_owned(),
            ),
        )?;
        let animation_b_votes = row.get::<_, Option<i32>>("animation_b_votes").ok_or(
            CalculateNewRoundMatchupsError::DbIntegrityError(
                "matchup has no animation B votes".to_owned(),
            ),
        )?;

        matchups.insert(
            row.get::<_, i32>("index"),
            Matchup {
                animation_a_id,
                animation_b_id,
                animation_a_votes,
                animation_b_votes,
            },
        );
    }

    for index in start_index..end_index {
        let matchup1 = matchups
            .get(&i32::try_from(index - x)?)
            .ok_or(CalculateNewRoundMatchupsError::MissingMatchup(index - x))?;
        let matchup1_winner = match matchup1.animation_a_votes.cmp(&matchup1.animation_b_votes) {
            Ordering::Greater => matchup1.animation_a_id.clone(),
            Ordering::Less => matchup1.animation_b_id.clone(),
            Ordering::Equal => {
                return Err(CalculateNewRoundMatchupsError::DbIntegrityError(
                    "matchup has equal votes".to_owned(),
                ))
            }
        };

        let matchup2 = matchups.get(&i32::try_from(index - x + 1)?).ok_or(
            CalculateNewRoundMatchupsError::MissingMatchup(index - x + 1),
        )?;
        let matchup2_winner = match matchup2.animation_a_votes.cmp(&matchup2.animation_b_votes) {
            Ordering::Greater => matchup2.animation_a_id.clone(),
            Ordering::Less => matchup2.animation_b_id.clone(),
            Ordering::Equal => {
                return Err(CalculateNewRoundMatchupsError::DbIntegrityError(
                    "matchup has equal votes".to_owned(),
                ))
            }
        };

        t.execute(
            r#"
            UPDATE "matchups"
            SET "animation_a_id" = $1, "animation_b_id" = $2
            WHERE "tournament_id" = $3 AND "index" = $4
            "#,
            &[
                &matchup1_winner,
                &matchup2_winner,
                &tournament_id,
                &i32::try_from(index)?,
            ],
        )
        .await
        .map_err(CalculateNewRoundMatchupsError::UpdateMatchupFailed)?;

        x -= 1;
    }
    Ok(())
}
