use std::{fmt, str::FromStr};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
pub(crate) use string_id;

string_id!(
    /// A vCard `UID`: the key that correlates one contact across both sides.
    /// Not PII — it may be logged.
    Uid
);

string_id!(
    /// Path of one card resource on its server, as returned by the server.
    Href
);

string_id!(
    /// Opaque entity tag, stored exactly as the server sent it.
    ETag
);

/// One of the two CardDAV endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    ICloud,
    Fastmail,
}

/// Which side wins when a card changed on both sides since the last sync.
pub type ConflictWinner = Side;

impl Side {
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::ICloud => Self::Fastmail,
            Self::Fastmail => Self::ICloud,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ICloud => "icloud",
            Self::Fastmail => "fastmail",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Side {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "icloud" => Ok(Self::ICloud),
            "fastmail" => Ok(Self::Fastmail),
            _ => Err(format!("expected `icloud` or `fastmail`, got `{s}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_other_flips() {
        assert_eq!(Side::ICloud.other(), Side::Fastmail);
        assert_eq!(Side::Fastmail.other(), Side::ICloud);
    }

    #[test]
    fn side_round_trips_through_str() {
        for side in [Side::ICloud, Side::Fastmail] {
            assert_eq!(side.to_string().parse::<Side>(), Ok(side));
        }
        assert_eq!(Side::ICloud.as_str(), "icloud");
        assert_eq!(Side::Fastmail.as_str(), "fastmail");
    }

    #[test]
    fn side_parse_is_case_insensitive() {
        assert_eq!("iCloud".parse::<Side>(), Ok(Side::ICloud));
        assert_eq!("FASTMAIL".parse::<Side>(), Ok(Side::Fastmail));
    }

    #[test]
    fn side_parse_error_names_both_choices() {
        assert_eq!("google".parse::<Side>(), Err("expected `icloud` or `fastmail`, got `google`".to_owned()));
    }

    #[test]
    fn string_ids_convert_and_display() {
        let uid = Uid::from("ABC-123");
        assert_eq!(uid.as_str(), "ABC-123");
        assert_eq!(uid.to_string(), "ABC-123");
        assert_eq!(Href::new(String::from("/card.vcf")).into_string(), "/card.vcf");
        assert_eq!(ETag::from("\"e1\"").as_str(), "\"e1\"");
    }
}
