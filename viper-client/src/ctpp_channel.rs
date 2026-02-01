use crate::command::Command;
use crate::command_response::{Actuator, Opendoor, VipConfig};

#[repr(u8)]
pub enum MessageType {
    OpenDoor = 0x00,        // valore da definire in base al tuo codice
    OpenDoorConfirm = 0x20, // valore da definire in base al tuo codice
}

// Helper function to convert a string to a buffer with optional null termination
fn string_to_buffer(s: &str, null_terminated: bool) -> Vec<u8> {
    let mut buffer = s.as_bytes().to_vec();
    if null_terminated {
        buffer.push(0x00);
    }
    buffer
}

const NULL: &[u8] = &[0x00];

#[derive(Debug)]
pub struct CTPPChannel {
    control: [u8; 2],
}

impl CTPPChannel {
    pub fn new(control: &[u8; 2]) -> CTPPChannel {
        CTPPChannel { control: *control }
    }

    pub fn open(&self, sub: &str) -> Vec<u8> {
        Command::channel(&String::from("CTPP"), &self.control, Some(sub.as_bytes()))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn get_unknown_open_door_message(&self, vip: &VipConfig) -> Vec<u8> {
        let apt_combined = format!("{}{}", vip.apt_address, vip.apt_subaddress);

        let req = [
            vec![0xc0, 0x18, 0x5c, 0x8b],
            vec![0x2b, 0x73, 0x00, 0x11],
            vec![0x00, 0x40, 0xac, 0x23],
            string_to_buffer(&apt_combined, true),
            vec![0x10, 0x0e],
            vec![0x00, 0x00, 0x00, 0x00],
            vec![0xff, 0xff, 0xff, 0xff],
            string_to_buffer(&apt_combined, true),
            string_to_buffer(&vip.apt_address, true),
            NULL.to_vec(),
        ]
        .concat();

        Command::make(&req, &self.control)
    }

    pub fn get_open_door_message(
        &self,
        vip: &VipConfig,
        door_item: &Opendoor,
        confirm: bool,
    ) -> Vec<u8> {
        let message_type = if confirm {
            MessageType::OpenDoorConfirm as u8
        } else {
            MessageType::OpenDoor as u8
        };

        let apt_with_output = format!("{}{}", vip.apt_address, door_item.output_index);

        let mut req = [
            vec![message_type],
            vec![0x5c, 0x8b],
            vec![0x2c, 0x74, 0x00, 0x00],
            vec![0xff, 0xff, 0xff, 0xff],
            string_to_buffer(&apt_with_output, true),
            string_to_buffer(&door_item.apt_address, true),
            NULL.to_vec(),
        ]
        .concat();
        if !req.len().is_multiple_of(2) {
            req.extend(NULL);
        }
        Command::make(&req, &self.control)
    }

    pub fn get_init_open_door_message(&self, vip: &VipConfig, door_item: &Opendoor) -> Vec<u8> {
        let apt_with_output = format!("{}{}", vip.apt_address, door_item.output_index);

        let mut req = [
            vec![0xc0, 0x18, 0x70, 0xab],
            vec![0x29, 0x9f, 0x00, 0x0d],
            vec![0x00, 0x2d],
            string_to_buffer(&door_item.apt_address, true),
            NULL.to_vec(),
            vec![door_item.output_index, 0x00, 0x00, 0x00],
            vec![0xff, 0xff, 0xff, 0xff],
            string_to_buffer(&apt_with_output, true),
            string_to_buffer(&door_item.apt_address, true),
            NULL.to_vec(),
        ]
        .concat();
        if !req.len().is_multiple_of(2) {
            req.extend(NULL);
        }
        Command::make(&req, &self.control)
    }

    pub fn get_init_open_actuator_message(
        &self,
        vip: &VipConfig,
        actuator_door_item: &Actuator,
    ) -> Vec<u8> {
        let apt_with_output = format!("{}{}", vip.apt_address, actuator_door_item.output_index);

        let mut req = [
            vec![0xc0, 0x18, 0x45, 0xbe],
            vec![0x8f, 0x5c, 0x00, 0x04],
            vec![0x00, 0x20, 0xff, 0x01],
            vec![0xff, 0xff, 0xff, 0xff],
            string_to_buffer(&apt_with_output, true),
            string_to_buffer(&actuator_door_item.apt_address, true),
            NULL.to_vec(),
        ]
        .concat();
        if !req.len().is_multiple_of(2) {
            req.extend(NULL);
        }
        Command::make(&req, &self.control)
    }

    pub fn get_open_actuator_message(
        &self,
        vip: &VipConfig,
        actuator_door_item: &Actuator,
        confirm: bool,
    ) -> Vec<u8> {
        let first_byte = if confirm { 0x20 } else { 0x00 };
        let apt_with_output = format!("{}{}", vip.apt_address, actuator_door_item.output_index);

        let mut req = [
            vec![first_byte, 0x18, 0x45, 0xbe],
            vec![0x8f, 0x5c, 0x00, 0x04],
            vec![0xff, 0xff, 0xff, 0xff],
            string_to_buffer(&apt_with_output, true),
            string_to_buffer(&actuator_door_item.apt_address, true),
            NULL.to_vec(),
        ]
        .concat();
        if !req.len().is_multiple_of(2) {
            req.extend(NULL);
        }
        Command::make(&req, &self.control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str;

    #[test]
    fn test_connect_open() {
        let ctpp = CTPPChannel::new(&[1, 2]);
        let conn = ctpp.open(&String::from("SB0000062"));

        assert_eq!(conn[2], 0x1e);
        assert_eq!(
            &conn[8..16],
            &[0xcd, 0xab, 0x01, 0x00, 0x07, 0x00, 0x00, 0x00]
        );
        assert_eq!(str::from_utf8(&conn[16..20]).unwrap(), "CTPP");
        assert_eq!(str::from_utf8(&conn[28..37]).unwrap(), "SB0000062");
        assert_eq!(conn[37], 0x00);
    }
}
