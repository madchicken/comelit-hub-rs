use crate::command::{Command, CommandKind};

pub struct Channel {
    command: String,
    control: [u8; 2],
    message_seq: u8,
}

impl Channel {
    pub fn new(control: &[u8; 2], command: &'static str) -> Channel {
        Channel {
            control: *control,
            command: command.to_string(),
            message_seq: 0,
        }
    }

    pub fn open(&self) -> Vec<u8> {
        Command::channel(&self.command, &self.control, None, Some(self.message_seq))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn com(&self, kind: CommandKind) -> Vec<u8> {
        Command::for_kind(kind, &self.control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_open() {
        let channel = Channel::new(&[0, 0], "INFO");
        channel.open();
    }
}
