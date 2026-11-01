use std::env;
use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub sqlite_path: PathBuf,
    pub jwt_secret: Vec<u8>,
    pub host: IpAddr,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(env::var_os)
    }

    fn from_vars(mut lookup: impl FnMut(&'static str) -> Option<OsString>) -> Result<Self, ConfigError> {
        let path = required(&mut lookup, "SQLITE_PATH")?;
        if path.is_empty() {
            return Err(ConfigError::invalid("SQLITE_PATH", "must not be empty"));
        }
        let sqlite_path = PathBuf::from(path);

        let jwt_secret = required(&mut lookup, "JWT_SECRET")?
            .into_string()
            .map_err(|_| ConfigError::invalid("JWT_SECRET", "must be valid UTF-8"))?
            .into_bytes();
        if jwt_secret.is_empty() {
            return Err(ConfigError::invalid("JWT_SECRET", "must not be empty"));
        }

        let host: IpAddr = required(&mut lookup, "HOST")?
            .into_string()
            .map_err(|_| ConfigError::invalid("HOST", "must be valid UTF-8"))?
            .parse()
            .map_err(|_| ConfigError::invalid("HOST", "must be an IP address"))?;

        let port: u16 = required(&mut lookup, "PORT")?
            .into_string()
            .map_err(|_| ConfigError::invalid("PORT", "must be valid UTF-8"))?
            .parse()
            .map_err(|_| ConfigError::invalid("PORT", "must be a valid port number"))?;

        Ok(Self {
            sqlite_path,
            jwt_secret,
            host,
            port,
        })
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

fn required(
    lookup: &mut impl FnMut(&'static str) -> Option<OsString>,
    name: &'static str,
) -> Result<OsString, ConfigError> {
    lookup(name).ok_or(ConfigError::Missing(name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid {
        name: &'static str,
        reason: &'static str,
    },
}

impl ConfigError {
    fn invalid(name: &'static str, reason: &'static str) -> Self {
        Self::Invalid { name, reason }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(formatter, "missing required environment variable {name}"),
            Self::Invalid { name, reason } => write!(formatter, "invalid {name}: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{Config, ConfigError};

    fn vars() -> HashMap<&'static str, OsString> {
        HashMap::from([
            ("SQLITE_PATH", OsString::from("/data/feed.db")),
            ("JWT_SECRET", OsString::from("secret")),
            ("HOST", OsString::from("127.0.0.1")),
            ("PORT", OsString::from("3000")),
        ])
    }

    fn parse(vars: HashMap<&'static str, OsString>) -> Result<Config, ConfigError> {
        Config::from_vars(move |name| vars.get(name).cloned())
    }

    #[test]
    fn reads_every_value() {
        let config = parse(vars()).unwrap();
        assert_eq!(config.sqlite_path, PathBuf::from("/data/feed.db"));
        assert_eq!(config.jwt_secret, b"secret".to_vec());
        assert_eq!(config.host.to_string(), "127.0.0.1");
        assert_eq!(config.port, 3000);
        assert_eq!(config.socket_addr().to_string(), "127.0.0.1:3000");
    }

    #[test]
    fn rejects_missing_values() {
        for name in ["SQLITE_PATH", "JWT_SECRET", "HOST", "PORT"] {
            let mut values = vars();
            values.remove(name);
            assert_eq!(parse(values), Err(ConfigError::Missing(name)));
        }
    }

    #[test]
    fn rejects_invalid_values() {
        let mut values = vars();
        values.insert("HOST", OsString::from("localhost"));
        assert_eq!(
            parse(values),
            Err(ConfigError::Invalid {
                name: "HOST",
                reason: "must be an IP address"
            })
        );

        let mut values = vars();
        values.insert("PORT", OsString::from("70000"));
        assert_eq!(
            parse(values),
            Err(ConfigError::Invalid {
                name: "PORT",
                reason: "must be a valid port number"
            })
        );

        let mut values = vars();
        values.insert("SQLITE_PATH", OsString::from(""));
        assert_eq!(
            parse(values),
            Err(ConfigError::Invalid {
                name: "SQLITE_PATH",
                reason: "must not be empty"
            })
        );

        let mut values = vars();
        values.insert("JWT_SECRET", OsString::from(""));
        assert_eq!(
            parse(values),
            Err(ConfigError::Invalid {
                name: "JWT_SECRET",
                reason: "must not be empty"
            })
        );
    }
}
