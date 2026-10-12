use rtspcam_audio::AudioError;

/// Voice close code: Discord requires DAVE (end-to-end encryption) and this client's version of
/// it was refused.
pub(crate) const CLOSE_DAVE_REQUIRED: u16 = 4017;
/// Voice close code: the bot was disconnected (kicked from the channel, or the channel was
/// deleted).
pub(crate) const CLOSE_DISCONNECTED: u16 = 4014;
/// Voice close code: the call was ended by Discord.
pub(crate) const CLOSE_CALL_TERMINATED: u16 = 4022;

/// Why something the bot tried failed. `Display` says what happened; [`hint`](Self::hint) says
/// what to do about it, like the pipeline's error kinds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotError {
    #[error("Discord refused the token")]
    BadToken,
    #[error("Discord refused the bot's intents")]
    Intents,
    #[error("can't reach Discord: {0}")]
    Network(String),
    #[error("joining the voice channel timed out")]
    JoinTimedOut,
    #[error("Discord refused the voice connection's encryption (close code {CLOSE_DAVE_REQUIRED})")]
    DaveRefused,
    #[error("the voice connection was closed (close code {0})")]
    VoiceClosed(u16),
    #[error("the bot was disconnected from the voice channel")]
    Kicked,
    #[error("the voice connection failed: {0}")]
    Voice(String),
    #[error("the sound source failed: {0}")]
    Input(AudioError),
    #[error("the output for hearing the channel failed: {0}")]
    Output(AudioError),
    #[error("{0}")]
    Other(String),
}

impl BotError {
    /// What the user can do about it.
    pub fn hint(&self) -> String {
        match self {
            Self::BadToken => {
                "The token was refused. Copy it again from the Developer Portal (Bot page, \
                 \"Reset Token\")."
                    .into()
            }
            Self::Intents => "Discord refused the gateway intents this bot asks for. \
                              RTSP Cam only needs the standard ones; check the bot's \
                              settings in the Developer Portal."
                .into(),
            Self::Network(_) => {
                "Check the internet connection. RTSP Cam keeps trying while logged in.".into()
            }
            Self::JoinTimedOut => "Check that the bot may see the channel, connect and speak \
                                   there (its role's permissions), and that the channel isn't \
                                   full."
                .into(),
            Self::DaveRefused => "Discord refused the voice connection because the \
                                  encryption (DAVE) is out of date. RTSP Cam needs an update."
                .into(),
            Self::VoiceClosed(code) => format!(
                "Discord closed the voice connection with code {code}. Try joining again; \
                 if it keeps happening, mention the code when reporting it."
            ),
            Self::Kicked => "Someone disconnected the bot, or the channel was deleted. \
                             Join again to go back."
                .into(),
            Self::Voice(_) => "Try joining again in a moment.".into(),
            Self::Input(_) => "Pick another sound source, or plug the device back in. \
                               RTSP Cam retries every few seconds."
                .into(),
            Self::Output(_) => "Pick another output, or plug the device back in. RTSP Cam                                 retries every few seconds."
                .into(),
            Self::Other(_) => "Try again. The log (Help > Open logs folder) has details.".into(),
        }
    }

    /// The error for a voice connection that Discord closed with `code`.
    pub fn from_close_code(code: u16) -> Self {
        match code {
            CLOSE_DAVE_REQUIRED => Self::DaveRefused,
            CLOSE_DISCONNECTED | CLOSE_CALL_TERMINATED => Self::Kicked,
            _ => Self::VoiceClosed(code),
        }
    }

    /// The error for a failed REST call with HTTP status `status`.
    pub fn from_http_status(status: u16, detail: String) -> Self {
        match status {
            401 | 403 => Self::BadToken,
            _ => Self::Other(format!("Discord answered {status}: {detail}")),
        }
    }

    /// Whether rejoining after this makes sense (a dropped connection), as opposed to giving
    /// up (kicked, refused).
    pub fn worth_retrying(&self) -> bool {
        match self {
            Self::Network(_) | Self::JoinTimedOut | Self::Voice(_) => true,
            Self::VoiceClosed(code) => !matches!(code, 4001..=4005 | 4011 | 4012 | 4016 | 4020),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dave_refusal_says_an_update_is_needed() {
        let e = BotError::from_close_code(4017);
        assert_eq!(e, BotError::DaveRefused);
        assert!(e.to_string().contains("4017"));
        assert!(e.hint().contains("needs an update"));
        assert!(!e.worth_retrying());
    }

    #[test]
    fn kicks_are_not_retried() {
        assert_eq!(BotError::from_close_code(4014), BotError::Kicked);
        assert_eq!(BotError::from_close_code(4022), BotError::Kicked);
        assert!(!BotError::Kicked.worth_retrying());
    }

    #[test]
    fn dropped_connections_are_retried() {
        assert!(BotError::from_close_code(4015).worth_retrying());
        assert!(BotError::from_close_code(4006).worth_retrying());
        assert!(BotError::Network("reset".into()).worth_retrying());
        assert!(BotError::JoinTimedOut.worth_retrying());
        assert!(!BotError::from_close_code(4004).worth_retrying());
    }

    #[test]
    fn a_refused_token_has_a_hint() {
        let e = BotError::from_http_status(401, "Unauthorized".into());
        assert_eq!(e, BotError::BadToken);
        assert!(e.hint().contains("Developer Portal"));
        assert!(matches!(
            BotError::from_http_status(500, "oops".into()),
            BotError::Other(_)
        ));
    }

    #[test]
    fn unknown_close_codes_are_shown() {
        let e = BotError::from_close_code(4999);
        assert!(e.to_string().contains("4999"));
        assert!(e.hint().contains("4999"));
    }
}
