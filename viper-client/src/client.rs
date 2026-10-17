use std::time::Duration;

use crate::{
    JSONResult, ViperError,
    channel::Channel,
    command::{CommandKind, PushInfo},
    command_response::{
        ActivateUserResponse, AuthResponse, ConfigurationResponse, InfoResponse, Opendoor,
        VipConfig,
    },
    ctpp_channel::CTPPChannel,
    helper::Helper,
    stream_wrapper::StreamWrapper,
    udp_stream_wrapper::UdpStreamWrapper,
    video::{
        self, CallMode, CallTarget, SpawnedUnit, UnitConfig, VideoOptions, VideoParams, VideoStats,
        ViperUnit, ctp::LogAddr,
    },
};
use serde::Deserialize;
use tracing::debug;

pub const ICONA_BRIDGE_PORT: u16 = 64100;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Lifetime of a subunit registration. The app asks for one hour and renews
/// it; a short lived client asks for less so that the apartment stops
/// forwarding calls to it soon after it is gone.
const REGISTRATION_TTL_SECS: u16 = 300;

/// Registration lifetime of a long lived unit; it renews the registration at
/// 90% of it, so a unit that disappears stops receiving calls within 10 min.
const UNIT_REGISTRATION_TTL_SECS: u16 = 600;

pub struct ViperClient {
    stream: StreamWrapper,
    udp: UdpStreamWrapper,
    control: [u8; 2],
}

impl ViperClient {
    pub async fn scan() -> Option<(String, u16)> {
        let scanner = comelit_client_rs::Scanner::scan(Some(Duration::from_secs(2))).await;
        if let Ok(devices) = scanner {
            devices
                .iter()
                .find(|d| d.app_id() == "HSrv" && d.address().is_some())
                .map(|d| {
                    let mut x = d.address().unwrap().split(":");
                    let ip = x.next().unwrap();
                    let port = x
                        .next()
                        .unwrap_or(ICONA_BRIDGE_PORT.to_string().as_str())
                        .parse()
                        .unwrap();
                    (ip.to_string(), port)
                })
        } else {
            None
        }
    }

    pub async fn new(ip: &str, port: u16) -> ViperClient {
        let doorbell = format!("{}:{}", ip, port);

        ViperClient {
            stream: StreamWrapper::new(doorbell),
            udp: UdpStreamWrapper::new(ip, port)
                .await
                .expect("Can't opend UDP connection"),
            control: Helper::control(),
        }
    }

    /// Like [`Self::new`], but reports an unreachable device as an error
    /// (after `CONNECT_TIMEOUT`) instead of panicking.
    pub async fn connect(ip: &str, port: u16) -> Result<ViperClient, ViperError> {
        let address = format!("{ip}:{port}");
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(&address))
            .await
            .map_err(|_| ViperError::Generic(format!("Timeout connecting to {address}")))??;
        Ok(ViperClient {
            stream: StreamWrapper::from_std(tcp.into_std()?)?,
            udp: UdpStreamWrapper::new(ip, port).await?,
            control: Helper::control(),
        })
    }

    pub fn sign_up(&mut self, email: &str) -> JSONResult<ActivateUserResponse> {
        let fact_channel = self.channel("FACT");
        self.stream.execute(&fact_channel.open())?;
        let activate_user = CommandKind::ActivateUser(String::from(email));
        let act_bytes = self.stream.execute(&fact_channel.com(activate_user))?;
        let json_response = Self::json(&act_bytes);

        self.stream.execute(&fact_channel.close())?;
        json_response
    }

    pub fn remove_all_users(&mut self, email: &String) -> JSONResult<serde_json::Value> {
        let fact_channel = self.channel("FACT");
        self.stream.execute(&fact_channel.open())?;
        let remove_all_users = CommandKind::RemoveAllUsers(String::from(email));
        let rem_bytes = self.stream.execute(&fact_channel.com(remove_all_users))?;
        self.stream.execute(&fact_channel.close())?;

        Self::json(&rem_bytes)
    }

    pub fn authorize(&mut self, token: &str) -> JSONResult<AuthResponse> {
        let uaut = CommandKind::UAUT(token.into());
        let uaut_channel = self.channel("UAUT");
        self.stream.execute(&uaut_channel.open())?;
        let uaut_bytes = self.stream.execute(&uaut_channel.com(uaut))?;

        let json_response = Self::json(&uaut_bytes);
        self.stream.execute(&uaut_channel.close())?;
        json_response
    }

    pub fn configuration(&mut self, addressbooks: &str) -> JSONResult<ConfigurationResponse> {
        let ucfg = CommandKind::UCFG(addressbooks.into());
        let ucfg_channel = self.channel("UCFG");
        self.stream.execute(&ucfg_channel.open())?;
        let ucfg_bytes = self.stream.execute(&ucfg_channel.com(ucfg))?;

        let str = String::from_utf8_lossy(&ucfg_bytes);
        debug!("Configuration response: {}", str);
        let json_response = Self::json(&ucfg_bytes);
        self.stream.execute(&ucfg_channel.close())?;
        json_response
    }

    pub fn info(&mut self) -> JSONResult<InfoResponse> {
        let info = CommandKind::INFO;
        let info_channel = self.channel("INFO");
        self.stream.execute(&info_channel.open())?;

        let info_bytes = self.stream.execute(&info_channel.com(info))?;
        let json_response = Self::json(&info_bytes);
        self.stream.execute(&info_channel.close())?;
        json_response
    }

    pub fn face_recognition_params(&mut self) -> JSONResult<serde_json::Value> {
        let frcg = CommandKind::FRCG;
        let frcg_channel = self.channel("FRCG");
        self.stream.execute(&frcg_channel.open())?;

        let frcg_bytes = self.stream.execute(&frcg_channel.com(frcg))?;
        let json_response = Self::json(&frcg_bytes);
        self.stream.execute(&frcg_channel.close())?;
        json_response
    }

    pub fn push_info(
        &mut self,
        apt_subaddress: u16,
        open_door: &Opendoor,
    ) -> JSONResult<serde_json::Value> {
        let push = CommandKind::PUSH(PushInfo {
            apt_address: open_door.apt_address.clone(),
            apt_subaddress: 50, // ??
            device_token: "87cd599d00bd7f83d01b85c67b28daa50314b142e7e6649accade61424131dd6".into(),
            profile_id: apt_subaddress.to_string(),
        });
        let push_channel = self.push_channel();
        self.stream.execute(&push_channel.open())?;

        let push_bytes = self.stream.execute(&push_channel.com(push))?;
        let json_response = Self::json(&push_bytes);
        self.stream.execute(&push_channel.close())?;
        json_response
    }

    pub fn open_door(&mut self, vip: &VipConfig, door_name: &str) -> Result<(), ViperError> {
        let sub = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let door_item = vip
            .user_parameters
            .opendoor_address_book
            .iter()
            .find(|d| d.name.as_str() == door_name)
            .ok_or(ViperError::Generic("Door not found".to_string()))?;

        let ctpp_channel = self.ctpp_channel();
        self.stream.execute(&ctpp_channel.open(&sub))?;
        debug!("CTPP Channel opened");

        self.stream
            .execute(&ctpp_channel.get_unknown_open_door_message(vip))?;
        debug!("Unknown sent");
        self.stream.read()?;
        debug!("Read 1");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, false))?;
        debug!("Open sent (false)");
        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, true))?;
        debug!("Open sent (true)");
        self.stream
            .execute_no_read(&ctpp_channel.get_init_open_door_message(vip, door_item))?;
        debug!("Init sent");
        self.stream.read()?;
        debug!("Read 2");
        self.stream.read()?;
        debug!("Read 3");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, false))?;
        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, true))?;

        // Close the remaining channels
        self.stream.execute(&ctpp_channel.close())?;
        Ok(())
    }

    pub fn open_actuator(&mut self, vip: &VipConfig, door_name: &str) -> Result<(), ViperError> {
        let sub = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let door_item = vip
            .user_parameters
            .actuator_address_book
            .iter()
            .find(|d| d.name.as_str() == door_name)
            .ok_or(ViperError::Generic("Actuator not found".to_string()))?;

        let ctpp_channel = self.ctpp_channel();
        self.stream.execute(&ctpp_channel.open(&sub))?;
        debug!("CTPP Channel opened");

        self.stream
            .execute(&ctpp_channel.get_unknown_open_door_message(vip))?;
        debug!("Unknown sent");
        self.stream.read()?;
        debug!("Read 1");

        self.stream
            .execute_no_read(&ctpp_channel.get_init_open_actuator_message(vip, door_item))?;
        self.stream.read()?;
        debug!("Read 2");
        self.stream.read()?;
        debug!("Read 3");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_actuator_message(vip, door_item, false))?;
        self.stream
            .execute_no_read(&ctpp_channel.get_open_actuator_message(vip, door_item, true))?;

        // Close the remaining channels
        self.stream.execute(&ctpp_channel.close())?;
        Ok(())
    }

    /// Watches an external unit ("auto-ignition") and records its video
    /// stream, see [`crate::video`] for the protocol. `target` is the name of
    /// an entrance or of a door in the address books of the configuration.
    /// The call is released after `options.duration` or when `stop` resolves.
    pub async fn start_video(
        &mut self,
        vip: &VipConfig,
        target: &str,
        options: VideoOptions,
        stop: impl Future<Output = ()>,
    ) -> Result<VideoStats, ViperError> {
        let target = CallTarget {
            src: Self::own_address(vip)?,
            mode: CallMode::Watch {
                external_unit: Self::entrance_address(vip, target)?,
            },
        };
        self.run_video(target, options, stop).await
    }

    /// Turns the connection into a long lived [`ViperUnit`] (see
    /// [`crate::video::unit`]): registered as a subunit of the apartment so
    /// that rings are reported, with media on demand from the ringing call
    /// or, when nobody rings, from `entrance` (an entrance or door name of
    /// the address books). Call after [`Self::authorize`].
    pub fn into_unit(
        mut self,
        vip: &VipConfig,
        entrance: Option<&str>,
        ring_timeout: Duration,
    ) -> Result<SpawnedUnit, ViperError> {
        let config = UnitConfig {
            src: Self::own_address(vip)?,
            apartment: Some(LogAddr::new(&vip.apt_address).map_err(ViperError::Generic)?),
            registration_ttl_secs: UNIT_REGISTRATION_TTL_SECS,
            entrance: entrance
                .map(|name| Self::entrance_address(vip, name))
                .transpose()?,
            ring_timeout,
            video: VideoParams::default(),
        };
        self.tick();
        let first_channel = u16::from_le_bytes(self.control);
        let tcp = self.stream.async_clone()?;
        Ok(ViperUnit::spawn(
            tcp,
            self.udp.socket(),
            first_channel,
            config,
        ))
    }

    /// Address of an entrance or door of the address books, by name.
    fn entrance_address(vip: &VipConfig, name: &str) -> Result<LogAddr, ViperError> {
        let book = &vip.user_parameters;
        let address = book
            .entrance_address_book
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.apt_address.as_str())
            .or_else(|| {
                book.opendoor_address_book
                    .iter()
                    .find(|d| d.name == name)
                    .map(|d| d.apt_address.as_str())
            })
            .ok_or_else(|| {
                let names: Vec<&str> = book
                    .entrance_address_book
                    .iter()
                    .map(|e| e.name.as_str())
                    .chain(book.opendoor_address_book.iter().map(|d| d.name.as_str()))
                    .collect();
                ViperError::Generic(format!("Unknown entrance {name:?}, available: {names:?}"))
            })?;
        LogAddr::new(address).map_err(ViperError::Generic)
    }

    /// Registers as a subunit of the apartment and waits for an incoming call
    /// (someone ringing), then records its early video without answering:
    /// the call keeps ringing on the other units. Unlike [`Self::start_video`]
    /// this works while the external unit is ringing, which refuses
    /// auto-ignition calls as busy. Stops when the call ends, after
    /// `ring_timeout` of ringing (then our leg of the call is released, like
    /// the app does; the other units keep ringing), after `options.duration`
    /// or when `stop` resolves.
    pub async fn listen_video(
        &mut self,
        vip: &VipConfig,
        ring_timeout: std::time::Duration,
        options: VideoOptions,
        stop: impl Future<Output = ()>,
    ) -> Result<VideoStats, ViperError> {
        let target = CallTarget {
            src: Self::own_address(vip)?,
            mode: CallMode::Listen {
                apartment: LogAddr::new(&vip.apt_address).map_err(ViperError::Generic)?,
                ttl_secs: REGISTRATION_TTL_SECS,
                ring_timeout,
            },
        };
        self.run_video(target, options, stop).await
    }

    fn own_address(vip: &VipConfig) -> Result<LogAddr, ViperError> {
        LogAddr::new(&format!("{}{}", vip.apt_address, vip.apt_subaddress))
            .map_err(ViperError::Generic)
    }

    async fn run_video(
        &mut self,
        target: CallTarget,
        options: VideoOptions,
        stop: impl Future<Output = ()>,
    ) -> Result<VideoStats, ViperError> {
        // The session needs the CTPP, UDPM and two RTPC channel ids
        self.tick();
        let first_channel = u16::from_le_bytes(self.control);
        for _ in 0..4 {
            self.tick();
        }

        let tcp = self.stream.async_clone()?;
        let result = video::run(tcp, self.udp.socket(), first_channel, target, options, stop).await;
        self.stream.restore_blocking()?;
        result
    }

    fn channel(&mut self, command: &'static str) -> Channel {
        self.tick();

        Channel::new(&self.control, command)
    }

    fn ctpp_channel(&mut self) -> CTPPChannel {
        self.tick();

        CTPPChannel::new(&self.control)
    }

    fn push_channel(&mut self) -> Channel {
        self.tick();

        Channel::new(&self.control, "PUSH")
    }

    fn json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> JSONResult<T> {
        match serde_json::from_slice(bytes) {
            Ok(json) => Ok(json),
            Err(e) => Err(ViperError::JSONError(e)),
        }
    }

    pub fn shutdown(&mut self) {
        self.stream.die();
    }

    // Move the control byte 1 ahead
    fn tick(&mut self) {
        self.control[0] += 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{command::Command, test_helper::SimpleTcpListener};
    use std::thread;

    #[tokio::test]
    async fn test_tick() {
        let _listener = SimpleTcpListener::new("127.0.0.1:3340");
        let mut client = ViperClient::new("127.0.0.1", 3340).await;

        let c = client.control;
        client.tick();

        assert_eq!(c[0] + 1, client.control[0])
    }

    #[tokio::test]
    async fn test_authorize() {
        let listener = SimpleTcpListener::new("127.0.0.1:3341");
        let mut client = ViperClient::new("127.0.0.1", 3341).await;

        thread::spawn(move || {
            let mocked_open = [
                0xcd, 0xab, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x1a, 0x12, 0x00, 0x00,
            ];

            let mocked_json = r#"{
                "message":"access",
                "message-type":"response",
                "message-id":5,
                "response-code":200,
                "response-string":"Access Granted"
            }"#;

            listener.mock_server(vec![
                Command::make(&mocked_open, &[0, 0]),
                Command::make(mocked_json.as_bytes(), &[0, 0]),
                Command::make(&[], &[0, 0]), // Closing the channel
            ])
        });

        let resp = client.authorize("TESTTOKEN").unwrap();
        assert_eq!(resp.response.response_string, "Access Granted");
        assert_eq!(resp.response.response_code, 200)
    }
}
