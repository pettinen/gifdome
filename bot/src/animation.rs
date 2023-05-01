use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    string::FromUtf8Error,
};

use frankenstein::{AsyncTelegramApi, GetFileParams};
use serde::Deserialize;

use crate::{API, CONFIG, POSSIBLE_DUPLICATES};

#[derive(Debug, thiserror::Error)]
pub enum AnimationParamsError {
    #[error("invalid frame count: {0}")]
    InvalidFrameCount(String),
    #[error("invalid frame rate: {0}")]
    InvalidFrameRate(String),
    #[error("no streams")]
    NoStreams,
}

#[derive(Deserialize)]
struct AnimationParamsStreamInput {
    width: u16,
    height: u16,
    r_frame_rate: String,
    nb_read_frames: String,
}

#[derive(Deserialize)]
struct AnimationParamsInput {
    streams: Vec<AnimationParamsStreamInput>,
}

#[derive(Debug)]
pub struct AnimationParams {
    pub width: i32,
    pub height: i32,
    pub fps_num: i32,
    pub fps_denom: i32,
    pub frames: i32,
}

impl TryFrom<AnimationParamsInput> for AnimationParams {
    type Error = AnimationParamsError;

    fn try_from(params: AnimationParamsInput) -> Result<Self, Self::Error> {
        let stream = params
            .streams
            .get(0)
            .ok_or(AnimationParamsError::NoStreams)?;

        let (fps_num, fps_denom) = match stream.r_frame_rate.split_once('/') {
            Some((num, denom)) => (num, denom),
            None => {
                return Err(AnimationParamsError::InvalidFrameRate(
                    stream.r_frame_rate.clone(),
                ))
            }
        };
        let (fps_num, fps_denom) = match (fps_num.parse(), fps_denom.parse()) {
            (Ok(num), Ok(denom)) => (num, denom),
            _ => {
                return Err(AnimationParamsError::InvalidFrameRate(
                    stream.r_frame_rate.clone(),
                ))
            }
        };

        let frames = match stream.nb_read_frames.parse() {
            Ok(frames) => frames,
            Err(_) => {
                return Err(AnimationParamsError::InvalidFrameCount(
                    stream.nb_read_frames.clone(),
                ))
            }
        };

        Ok(Self {
            width: stream.width.into(),
            height: stream.height.into(),
            fps_num,
            fps_denom,
            frames,
        })
    }
}

impl AnimationParams {
    pub fn duration(&self) -> f64 {
        self.frames as f64 * self.fps_denom as f64 / self.fps_num as f64
    }
}

fn shell_quote_path(path: &Path) -> Option<String> {
    path.to_str()
        .map(shell_quote::bash::quote)
        .map(|s| s.into_string().ok())
        .flatten()
}

#[derive(Debug, thiserror::Error)]
pub enum GetAnimationParamsError {
    #[error("error running ffprobe")]
    CommandError(#[from] std::io::Error),
    #[error("invalid animation parameters: {0}")]
    InvalidParams(#[from] AnimationParamsError),
    #[error("ffmpeg or ffprobe exited with nonzero status: {0}\nstdout: {1}\nstderr: {2}")]
    NonZeroStatus(ExitStatus, String, String),
    #[error("file path is not UTF-8")]
    NonUtf8Path,
    #[error("could not parse ffprobe output as JSON")]
    JsonParseError(#[from] serde_json::Error),
}

pub async fn get_animation_params(
    animation_id: &str,
) -> Result<AnimationParams, GetAnimationParamsError> {
    let config = CONFIG.wait();
    let path = shell_quote_path(&config.animation.save_dir.join(animation_id))
        .ok_or(GetAnimationParamsError::NonUtf8Path)?;

    let command = format!(
        "ffmpeg -v quiet -i {path} -map 0:v:0 -c copy -f matroska - | \
            ffprobe -v quiet -print_format json -show_streams -count_frames \
                -show_entries stream=width,height,r_frame_rate,nb_read_frames -
        ",
    );
    let output = Command::new("bash")
        .arg("-o")
        .arg("pipefail")
        .arg("-c")
        .arg(command)
        .output()?;

    if !output.status.success() {
        return Err(GetAnimationParamsError::NonZeroStatus(
            output.status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    match serde_json::from_slice::<AnimationParamsInput>(&output.stdout) {
        Ok(params) => match params.try_into() {
            Ok(params) => Ok(params),
            Err(err) => Err(err.into()),
        },
        Err(err) => Err(GetAnimationParamsError::JsonParseError(err)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateThumbnailError {
    #[error("error running ffmpeg: {0}")]
    CommandError(#[from] std::io::Error),
    #[error("file path is not UTF-8")]
    NonUtf8Path,
    #[error("ffmpeg exited with nonzero status: {0}\nstdout: {1}\nstderr: {2}")]
    FfmpegNonZeroStatus(ExitStatus, String, String),
}

pub fn generate_thumbnail(animation_id: &str) -> Result<(), GenerateThumbnailError> {
    let config = CONFIG.wait();
    let animation_path = config.animation.save_dir.join(animation_id);
    let animation_path = animation_path
        .to_str()
        .ok_or(GenerateThumbnailError::NonUtf8Path)?;
    let thumbnail_path = config.animation.thumbnail_save_dir.join(animation_id);
    let thumbnail_path = thumbnail_path
        .to_str()
        .ok_or(GenerateThumbnailError::NonUtf8Path)?;

    let output = Command::new("ffmpeg")
        .args(&[
            "-v",
            "warning",
            "-y",
            "-i",
            animation_path,
            "-filter:v",
            r"select=eq(n\,0)",
            "-codec:v",
            "png",
            "-f",
            "image2pipe",
            thumbnail_path,
        ])
        .output()?;
    if !output.status.success() {
        return Err(GenerateThumbnailError::FfmpegNonZeroStatus(
            output.status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    tokio::spawn(async {
        match find_duplicates() {
            Ok(duplicates) => {
                let mut global_value = POSSIBLE_DUPLICATES.lock().await;
                *global_value = duplicates;
            }
            Err(err) => eprintln!("failed to find duplicates: {err}"),
        }
    });

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateDuplicatesError {
    #[error("error running findimagedupes: {0}")]
    CommandError(#[from] std::io::Error),
    #[error("file path is not UTF-8")]
    NonUtf8Path,
    #[error("findimagedupes output is not UTF-8: {0}")]
    NonUtf8Output(#[from] FromUtf8Error),
    #[error("findimagedupes exited with nonzero status: {0}\nstdout: {1}\nstderr: {2}")]
    NonZeroStatus(ExitStatus, String, String),
}

pub fn find_duplicates() -> Result<Vec<HashSet<String>>, UpdateDuplicatesError> {
    let config = CONFIG.wait();

    let fingerprint_file = config
        .animation
        .thumbnail_fingerprint_file
        .to_str()
        .ok_or(UpdateDuplicatesError::NonUtf8Path)?;

    let output = Command::new("findimagedupes")
        .args(&[
            "--fingerprints",
            fingerprint_file,
            "--threshold",
            &config.animation.thumbnail_fingerprint_threshold,
            config
                .animation
                .thumbnail_save_dir
                .to_str()
                .ok_or(UpdateDuplicatesError::NonUtf8Path)?,
        ])
        .output()?;
    if !output.status.success() {
        return Err(UpdateDuplicatesError::NonZeroStatus(
            output.status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|file_path| {
                    Path::new(file_path)
                        .file_name()
                        .map(|file_name| file_name.to_str().map(|file_name| file_name.to_string()))
                        .flatten()
                })
                .flatten()
                .collect()
        })
        .collect())
}

#[derive(Debug, thiserror::Error)]
pub enum SaveAnimationError {
    #[error("api error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error("api response missing file path")]
    ApiResponseMissingFilePath,
    #[error("api response missing size")]
    ApiResponseMissingSize,
    #[error("download error: {0}")]
    DownloadError(#[from] reqwest::Error),
    #[error("invalid URI: {0}")]
    InvalidUri(#[from] hyper::http::uri::InvalidUri),
    #[error("animation too large ({0} bytes)")]
    TooLarge(u64),
    #[error("could not write file: {0}")]
    WriteError(#[from] std::io::Error),
}

pub async fn save_animation(
    animation_id: &str,
    file_identifier: &str,
) -> Result<(), SaveAnimationError> {
    let api = API.wait();
    let config = CONFIG.wait();
    let file = api
        .get_file(&GetFileParams::builder().file_id(file_identifier).build())
        .await?
        .result;
    let download_path = match file.file_path {
        Some(file_path) => file_path,
        None => return Err(SaveAnimationError::ApiResponseMissingFilePath),
    };
    match file.file_size {
        Some(size) => {
            if size > config.animation.max_size_bytes {
                return Err(SaveAnimationError::TooLarge(size));
            }
        }
        None => return Err(SaveAnimationError::ApiResponseMissingSize),
    }
    let res = reqwest::get(format!(
        "https://api.telegram.org/file/bot{token}/{download_path}",
        token = config.bot.token,
    ))
    .await?;
    let save_path = config.animation.save_dir.join(animation_id);
    std::fs::write(&save_path, res.bytes().await?)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CombineAnimationsError {
    #[error("failed to run commands to combine animations: {0}")]
    CommandError(#[source] std::io::Error),
    #[error("file path is not UTF-8")]
    NonUtf8Path,
    #[error("vspipe or x264 exited with nonzero status: {0}")]
    NonZeroStatus(ExitStatus),
}

pub async fn combine_animations(a_id: &str, b_id: &str) -> Result<PathBuf, CombineAnimationsError> {
    let config = CONFIG.wait();

    let a_path = shell_quote_path(&config.animation.save_dir.join(a_id))
        .ok_or(CombineAnimationsError::NonUtf8Path)?;
    let b_path = shell_quote_path(&config.animation.save_dir.join(b_id))
        .ok_or(CombineAnimationsError::NonUtf8Path)?;

    let out_filename = format!("{a_id}.{b_id}.mp4");
    let out_path = config.animation.temp_save_dir.join(out_filename);
    let out_path_quoted = shell_quote_path(&out_path).ok_or(CombineAnimationsError::NonUtf8Path)?;

    _ = std::fs::remove_file(&out_path);

    let command = format!(
        "vspipe -c y4m -a a={a_path} -a b={b_path} combine.vpy - | \
         x264 --demuxer y4m --muxer mp4 --crf 30 --preset ultrafast --output {out_path_quoted} -",
    );
    let output = Command::new("bash")
        .current_dir(&config.animation.vspipe_working_dir)
        .arg("-o")
        .arg("pipefail")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(CombineAnimationsError::CommandError)?;

    if !output.status.success() {
        eprintln!("{:?}", std::str::from_utf8(&output.stdout));
        eprintln!("{:?}", std::str::from_utf8(&output.stderr));
        return Err(CombineAnimationsError::NonZeroStatus(output.status));
    }
    Ok(out_path)
}
