use std::path::PathBuf;

use anyhow::Context;
use aws_config::{
    BehaviorVersion,
    profile::profile_file::{ProfileFileKind, ProfileFiles},
};
use aws_sdk_polly::{
    Client,
    types::{Engine, OutputFormat, TextType, VoiceId},
};
use aws_types::region::Region;

use crate::bedrock::expand_home_path;

#[derive(Clone, Debug, PartialEq)]
pub struct PollySpeechSettings {
    pub region: String,
    pub profile: Option<String>,
    pub config_file: Option<PathBuf>,
    pub shared_credentials_file: Option<PathBuf>,
    pub default_voice_id: String,
}

#[derive(Clone, Debug)]
pub struct PollySpeechService {
    client: Client,
    settings: PollySpeechSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpeechSynthesisOutput {
    pub audio: Vec<u8>,
    pub content_type: String,
    pub voice_id: String,
}

impl PollySpeechService {
    pub async fn new(settings: PollySpeechSettings) -> anyhow::Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()));
        if let Some(profile_files) = build_profile_files(&settings)? {
            loader = loader.profile_files(profile_files);
        }
        if let Some(profile) = settings.profile.clone() {
            loader = loader.profile_name(profile);
        }
        let sdk_config = loader.load().await;
        Ok(Self {
            client: Client::new(&sdk_config),
            settings,
        })
    }

    pub async fn synthesize(
        &self,
        text: &str,
        requested_voice_id: Option<&str>,
    ) -> anyhow::Result<SpeechSynthesisOutput> {
        let voice_id_raw = requested_voice_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.settings.default_voice_id)
            .trim()
            .to_string();
        let voice_id = VoiceId::from(voice_id_raw.as_str());
        let response = self
            .client
            .synthesize_speech()
            .engine(Engine::Neural)
            .output_format(OutputFormat::Mp3)
            .text(text)
            .text_type(TextType::Text)
            .voice_id(voice_id)
            .send()
            .await
            .with_context(|| format!("failed to synthesize speech with voice `{voice_id_raw}`"))?;
        let collected = response
            .audio_stream
            .collect()
            .await
            .context("failed to read Polly audio stream")?;
        Ok(SpeechSynthesisOutput {
            audio: collected.into_bytes().to_vec(),
            content_type: "audio/mpeg".to_string(),
            voice_id: voice_id_raw,
        })
    }
}

#[allow(deprecated)]
fn build_profile_files(settings: &PollySpeechSettings) -> anyhow::Result<Option<ProfileFiles>> {
    let config_file = settings
        .config_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;
    let shared_credentials_file = settings
        .shared_credentials_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;

    if config_file.is_none() && shared_credentials_file.is_none() {
        return Ok(None);
    }

    let mut builder = ProfileFiles::builder();
    if let Some(path) = config_file {
        builder = builder.with_file(ProfileFileKind::Config, path);
    } else {
        builder = builder.include_default_config_file(true);
    }

    if let Some(path) = shared_credentials_file {
        builder = builder.with_file(ProfileFileKind::Credentials, path);
    } else {
        builder = builder.include_default_credentials_file(true);
    }

    Ok(Some(builder.build()))
}
