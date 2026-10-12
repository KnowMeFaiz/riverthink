/// The three universal channels. All three are institutionally equal:
/// nothing in the code may branch on *which* channel in a way that grants
/// one of them extra authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    E,
    Z,
    D,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::E, Channel::Z, Channel::D];

    pub fn code(self) -> &'static str {
        match self {
            Channel::E => "E",
            Channel::Z => "Z",
            Channel::D => "D",
        }
    }

    pub fn parse(s: &str) -> Option<Channel> {
        match s.trim().to_ascii_uppercase().as_str() {
            "E" => Some(Channel::E),
            "Z" => Some(Channel::Z),
            "D" => Some(Channel::D),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for c in Channel::ALL {
            assert_eq!(Channel::parse(c.code()), Some(c));
            assert_eq!(Channel::parse(&c.code().to_lowercase()), Some(c));
        }
        assert_eq!(Channel::parse("X"), None);
        assert_eq!(Channel::parse(""), None);
    }
}
