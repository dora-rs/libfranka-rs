// Copyright (c) 2021 Marco Boneberger
// Licensed under the EUPL-1.2-or-later
#![allow(dead_code)]

extern crate libc;
extern crate nix;

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::mem::size_of;
use std::net::TcpStream as StdTcpStream;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::str::FromStr;
use std::time::Duration;

use mio::net::{TcpStream, UdpSocket};
use mio::{Events, Interest, Poll, Token};

use nix::sys::socket::setsockopt;
use nix::sys::socket::sockopt::{KeepAlive, TcpKeepCount, TcpKeepIdle, TcpKeepInterval};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::exception::{FrankaException, FrankaResult};
use crate::gripper::types::{
    CommandHeader,
};
use crate::robot::errors::FrankaErrors;
use crate::robot::logger::Record;
use crate::robot::robot_state::AbstractRobotState;
use crate::robot::service_types::{
    ConnectRequest, LoadModelLibraryRequest,
    LoadModelLibraryStatus, MoveRequest, PandaCommandEnum, PandaCommandHeader, RobotHeader,
    SetCartesianImpedanceRequest, SetCollisionBehaviorRequest,
    SetEeToKRequest,
    SetGuidingModeRequest, SetJointImpedanceRequest, SetLoadRequest, SetNeToEeRequest,
};
use crate::robot::types::AbstractRobotStateIntern;
use crate::{RobotModel, RobotState};

const CLIENT: Token = Token(1);

pub enum NetworkType {
    Panda,
    Fr3,
    Gripper,
}

pub trait DeviceData {
    type CommandHeader: CommandHeader;
    type CommandEnum;
    fn create_header(
        command_id: &mut u32,
        command: Self::CommandEnum,
        size: usize,
    ) -> Self::CommandHeader;
    fn get_library_version() -> u16;
}

pub trait RobotData: DeviceData {
    type DeviceData: DeviceData;
    type Header: RobotHeader;
    type State: AbstractRobotState + From<Self::StateIntern> + From<RobotState>;
    type StateIntern: Debug + DeserializeOwned + Serialize + AbstractRobotStateIntern + 'static;
    type Model: RobotModel;
    type LoadModelRequestWithHeader: MessageCommand
        + Serialize
        + From<(u32, LoadModelLibraryRequest)>;
    type SetCollisionBehaviorRequestWithHeader: MessageCommand
        + Serialize
        + From<(u32, SetCollisionBehaviorRequest)>;
    type SetLoadRequestWithHeader: MessageCommand + Serialize + From<(u32, SetLoadRequest)>;
    type SetJointImpedanceRequestWithHeader: MessageCommand
        + Serialize
        + From<(u32, SetJointImpedanceRequest)>;
    type SetCartesianImpedanceRequestWithHeader: MessageCommand
        + Serialize
        + From<(u32, SetCartesianImpedanceRequest)>;
    type SetGuidingModeRequestWithHeader: MessageCommand
        + Serialize
        + From<(u32, SetGuidingModeRequest)>;
    type ConnectRequestWithHeader: MessageCommand + Serialize + From<(u32, ConnectRequest)>;
    type SetEeToKRequestWithHeader: MessageCommand + Serialize + From<(u32, SetEeToKRequest)>;
    type SetNeToEeRequestWithHeader: MessageCommand + Serialize + From<(u32, SetNeToEeRequest)>;
    type MoveRequestWithHeader: MessageCommand + Serialize + From<(u32, MoveRequest)>;
    type MoveStatus: DeserializeOwned + PartialEq<Self::MoveStatus> + Copy + Clone + 'static; // todo is this static fine here?
    type GetterSetterStatus: DeserializeOwned + Copy + Clone + 'static; // todo is this static fine here?
    type StopMoveStatus: DeserializeOwned + Copy + Clone + 'static; // todo is this static fine here?
    type AutomaticErrorRecoveryStatus: DeserializeOwned + Copy + Clone + 'static; // todo is this static fine here?

    fn create_model_library_request(
        command_id: &mut u32,
        request: LoadModelLibraryRequest,
    ) -> Self::LoadModelRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }
    fn create_set_collision_behavior_request(
        command_id: &mut u32,
        request: SetCollisionBehaviorRequest,
    ) -> Self::SetCollisionBehaviorRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }
    fn create_set_load_request(
        command_id: &mut u32,
        request: SetLoadRequest,
    ) -> Self::SetLoadRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_set_joint_impedance_request(
        command_id: &mut u32,
        request: SetJointImpedanceRequest,
    ) -> Self::SetJointImpedanceRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_set_cartesian_impedance_request(
        command_id: &mut u32,
        request: SetCartesianImpedanceRequest,
    ) -> Self::SetCartesianImpedanceRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_set_guiding_mode_request(
        command_id: &mut u32,
        request: SetGuidingModeRequest,
    ) -> Self::SetGuidingModeRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_set_ee_to_k_request(
        command_id: &mut u32,
        request: SetEeToKRequest,
    ) -> Self::SetEeToKRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_set_ne_to_ee_request(
        command_id: &mut u32,
        request: SetNeToEeRequest,
    ) -> Self::SetNeToEeRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_move_request(
        command_id: &mut u32,
        request: MoveRequest,
    ) -> Self::MoveRequestWithHeader {
        *command_id += 1;
        (*command_id - 1, request).into()
    }

    fn create_connect_request(
        command_id: &mut u32,
        udp_port: u16,
    ) -> Self::ConnectRequestWithHeader;

    fn create_automatic_error_recovery_request(command_id: &mut u32) -> Self::CommandHeader;
    fn create_stop_request(command_id: &mut u32) -> Self::CommandHeader;

    fn handle_command_move_status(status: Self::MoveStatus) -> Result<(), FrankaException>;
    fn create_control_exception(
        message: String,
        move_status: Self::MoveStatus,
        reflex_reasons: &FrankaErrors,
        log: Vec<Record<Self::State>>,
    ) -> FrankaException;
    fn create_control_exception_if_reflex_aborted(
        message: String,
        move_status: Self::MoveStatus,
        reflex_reasons: &FrankaErrors,
        log: Vec<Record<Self::State>>,
    ) -> FrankaResult<()>;

    fn handle_getter_setter_status(status: Self::GetterSetterStatus) -> FrankaResult<()>;
    fn handle_automatic_error_recovery_status(
        status: Self::AutomaticErrorRecoveryStatus,
    ) -> FrankaResult<()>;
    fn handle_command_stop_move_status(status: Self::StopMoveStatus)
        -> Result<(), FrankaException>;
}

pub trait MessageCommand {
    fn get_command_message_id(&self) -> u32;
}

pub struct Network<Data: DeviceData> {
    tcp_socket: TcpStream,
    udp_socket: UdpSocket,
    udp_server_address: SocketAddr,
    udp_port: u16,
    udp_timeout: Duration,
    pub command_id: u32,
    pending_response: Vec<u8>,
    pending_response_offset: usize,
    pending_response_len: usize,
    pending_command_id: u32,
    received_responses: HashMap<u32, Vec<u8>>,
    poll_read: Poll,
    events: Events,
    poll_read_udp: Poll,
    events_udp: Events,
    data: PhantomData<Data>,
}

impl<Data: DeviceData> Network<Data> {
    pub fn new(franka_address: &str, franka_port: u16) -> Result<Network<Data>, Box<dyn Error>> {
        let address_str: String = format!("{}:{}", franka_address, franka_port);
        let sock_address = address_str.to_socket_addrs().unwrap().next().unwrap();
        let mut tcp_socket = TcpStream::from_std(StdTcpStream::connect(sock_address)?);
        let fd = tcp_socket.as_raw_fd();

        setsockopt(fd, KeepAlive, &true)?;
        setsockopt(fd, TcpKeepIdle, &1)?;
        setsockopt(fd, TcpKeepCount, &3)?;
        setsockopt(fd, TcpKeepInterval, &1)?;

        let udp_timeout = Duration::from_secs(1); // TODO: offer in constructor
        let ip_addr = IpAddr::from_str("0.0.0.0")?;
        let udp_server_address = SocketAddr::new(ip_addr, 0);

        let mut udp_socket = UdpSocket::bind(udp_server_address)?;
        let udp_port = udp_socket.local_addr()?.port();

        let command_id = 0;
        let pending_response: Vec<u8> = Vec::new();
        let pending_response_offset = 0;
        let pending_command_id = 0;
        let received_responses = HashMap::new();
        let poll_read = Poll::new()?;
        poll_read
            .registry()
            .register(&mut tcp_socket, CLIENT, Interest::READABLE)?;
        let poll_read_udp = Poll::new()?;
        poll_read_udp
            .registry()
            .register(&mut udp_socket, CLIENT, Interest::READABLE)?;
        let events = Events::with_capacity(128);
        let events_udp = Events::with_capacity(1);
        Ok(Network {
            tcp_socket,
            udp_socket,
            udp_server_address,
            udp_port,
            udp_timeout,
            command_id,
            pending_response,
            pending_response_offset,
            pending_command_id,
            pending_response_len: 0,
            received_responses,
            poll_read,
            events,
            poll_read_udp,
            events_udp,
            data: PhantomData,
        })
    }

    pub fn create_header_for_panda(
        &mut self,
        command: PandaCommandEnum,
        size: usize,
    ) -> PandaCommandHeader {
        let header = PandaCommandHeader::new(command, self.command_id, size as u32);
        self.command_id += 1;
        header
    }

    pub fn create_header(
        &mut self,
        command: Data::CommandEnum,
        size: usize,
    ) -> Data::CommandHeader {
        Data::create_header(&mut self.command_id, command, size)
    }

    pub fn tcp_send_request<T: Serialize + MessageCommand>(&mut self, request: T) -> u32 {
        let encoded_request = serialize(&request);
        self.tcp_socket.write_all(&encoded_request).unwrap();
        request.get_command_message_id()
    }
    /// Blocks until a Response message with the given command ID has been received and returns this
    /// response.
    ///
    /// # Arguments
    /// * `command_id` - Expected command ID of the Response.
    pub fn tcp_blocking_receive_response<T: DeserializeOwned + 'static>(
        &mut self,
        command_id: u32,
    ) -> T {
        let response_bytes = self.wait_for_response_to_arrive(&command_id);
        deserialize(&response_bytes)
    }
    /// Blocks until a Response message with the given command ID has been received and returns this
    /// response.
    ///
    /// # Arguments
    /// * `command_id` - Expected command ID of the Response.
    pub fn tcp_blocking_receive_status<T: DeserializeOwned + 'static>(
        &mut self,
        command_id: u32,
    ) -> T {
        let response_bytes = self.wait_for_response_to_arrive(&command_id);
        let (_, out): (Data::CommandHeader, T) = deserialize(&response_bytes);
        out
    }
    /// Blocks until a LoadModelLibraryResponse message with the given command ID has been received
    /// and returns this LoadModelLibraryResponse.
    /// # Arguments
    /// * `command_id` - Expected command ID of the Response.
    /// * `buffer` -  variable-length data for the expected LoadModelLibraryResponse message (if
    /// any has been received) is copied into it.
    ///
    /// # Error
    /// * [`ModelException`](`crate::exception::FrankaException::ModelException`) - if the
    /// model could not be downloaded successfully.
    ///
    pub fn tcp_blocking_receive_load_library_response(
        &mut self,
        command_id: u32,
        buffer: &mut Vec<u8>,
    ) -> FrankaResult<LoadModelLibraryStatus> {
        let response_bytes = self.wait_for_response_to_arrive(&command_id);
        let (header, status): (Data::CommandHeader, LoadModelLibraryStatus) = deserialize(
            &response_bytes
                [0..size_of::<LoadModelLibraryStatus>() + size_of::<Data::CommandHeader>()],
        );
        match status {
            LoadModelLibraryStatus::Success => {}
            LoadModelLibraryStatus::Error => {
                return Err(FrankaException::ModelException {
                    message: "libfranka-rs: Server reports error when loading model library."
                        .to_string(),
                });
            }
        }
        assert_ne!(
            header.get_size() as usize,
            size_of::<LoadModelLibraryStatus>() + size_of::<Data::CommandHeader>()
        );
        buffer.append(&mut Vec::from(
            &response_bytes
                [size_of::<LoadModelLibraryStatus>() + size_of::<Data::CommandHeader>()..],
        ));
        Ok(status)
    }
    /// Tries to receive a Response message with the given command ID (non-blocking).
    ///
    /// # Arguments
    /// * `command_id` - Expected command ID of the response
    /// * `handler` -  Callback to be invoked if the expected response has been received.
    ///
    /// # Return
    /// * true - if everything worked as expected
    /// * false - if the message could not be received
    ///
    /// # Error
    /// * [`FrankaException`](`crate::exception::FrankaException`) - if the handler returns an exception
    pub fn tcp_receive_response<T, F>(
        &mut self,
        command_id: u32,
        handler: F,
    ) -> Result<bool, FrankaException>
    where
        F: FnOnce(T) -> Result<(), FrankaException>,
        T: DeserializeOwned + 'static,
    {
        self.tcp_read_from_buffer(Duration::from_micros(0));
        let message = self.received_responses.get(&command_id);
        if message.is_none() {
            return Ok(false);
        }
        if message.unwrap().len() != size_of::<T>() + size_of::<Data::CommandHeader>() {
            panic!("libfranka-rs: Incorrect TCP message size.");
        }
        let message: (Data::CommandHeader, T) = deserialize(message.unwrap());
        let result = handler(message.1);
        match result {
            Ok(_) => {
                self.received_responses.remove(&command_id);
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn wait_for_response_to_arrive(&mut self, command_id: &u32) -> Vec<u8> {
        let mut response_bytes: Option<Vec<u8>> = None;
        while response_bytes.is_none() {
            {
                self.tcp_read_from_buffer(Duration::from_millis(10));
                response_bytes = self.received_responses.remove(command_id);
            }
            std::thread::yield_now();
        }
        response_bytes.unwrap()
    }
    pub fn udp_receive<T: Debug + Serialize + DeserializeOwned + 'static>(&mut self) -> Option<T> {
        // TODO replace Vec<u8> with array when https://github.com/rust-lang/rust/issues/43408
        // is fixed
        let mut buffer: Vec<u8> = vec![0; size_of::<T>()];
        let available_bytes = self.udp_socket.peek(&mut buffer).ok()?;
        if available_bytes >= size_of::<T>() {
            let object: Option<T> = match self.udp_blocking_receive() {
                Ok(o) => Some(o),
                Err(_) => None,
            };
            return object;
        }
        None
    }
    pub fn udp_blocking_receive<T: Debug + Serialize + DeserializeOwned + 'static>(
        &mut self,
    ) -> FrankaResult<T> {
        self.poll_read_udp
            .poll(&mut self.events_udp, Some(self.udp_timeout))
            .unwrap();
        for event in self.events_udp.iter() {
            match event.token() {
                CLIENT => {
                    if event.is_readable() {
                        let mut buffer: Vec<u8> = vec![0; size_of::<T>()];
                        let read_bytes_and_address = self.udp_socket.recv_from(&mut buffer);
                        while self.udp_socket.recv_from(&mut buffer).is_ok() {}
                        let read_bytes = match read_bytes_and_address {
                            Ok(res) => {
                                self.udp_server_address = res.1;
                                res.0
                            }
                            Err(e) => {
                                return Err(FrankaException::NetworkException {
                                    message: e.to_string(),
                                });
                            }
                        };
                        if read_bytes != size_of::<T>() {
                            return Err(FrankaException::NetworkException { message: format!("UDP object could not be received: object has {} bytes but it should have {} bytes", read_bytes, size_of::<T>()) });
                        }
                        return Ok(deserialize(&buffer));
                    }
                }
                _ => unreachable!(),
            }
        }
        Err(FrankaException::NetworkException {
            message: "libfranka-rs: UDP receive: timeout".to_string(),
        })
    }
    pub fn udp_send<T: Debug + Serialize + DeserializeOwned>(
        &mut self,
        data: &T,
    ) -> FrankaResult<()> {
        let bytes_send = self
            .udp_socket
            .send_to(&serialize(data), self.udp_server_address)
            .map_err(|e| FrankaException::NetworkException {
                message: e.to_string(),
            })?;
        if bytes_send != size_of::<T>() {
            return Err(FrankaException::NetworkException {
                message: "libfranka-rs: UDP object could not be send".to_string(),
            });
        }
        Ok(())
    }

    fn tcp_read_from_buffer(&mut self, timeout: Duration) {
        self.poll_read
            .poll(&mut self.events, Some(timeout))
            .unwrap();
        for event in self.events.iter() {
            match event.token() {
                CLIENT => {
                    if event.is_readable() {
                        let mut buffer = [0_u8; 150000];
                        let available_bytes = self.tcp_socket.peek(&mut buffer);
                        let available_bytes = match available_bytes {
                            Ok(a) => a,
                            Err(e) => {
                                eprintln!("{}", e);
                                return;
                            }
                        };

                        if self.pending_response.is_empty() {
                            let header_mem_size = size_of::<Data::CommandHeader>();
                            if available_bytes >= header_mem_size {
                                let mut header_bytes: Vec<u8> = vec![0; header_mem_size];
                                self.tcp_socket.read_exact(&mut header_bytes).unwrap();
                                self.pending_response.append(&mut header_bytes.clone());
                                self.pending_response_offset = header_mem_size;
                                let header: Data::CommandHeader = deserialize(&header_bytes);
                                self.pending_response_len = header.get_size() as usize;
                                self.pending_command_id = header.get_command_id();
                            }
                        }
                        if !self.pending_response.is_empty() && available_bytes > 0 {
                            let number_of_bytes_to_read = usize::min(
                                available_bytes,
                                self.pending_response_len - self.pending_response_offset,
                            );
                            let mut response_buffer: Vec<u8> = vec![0; number_of_bytes_to_read];
                            self.tcp_socket.read_exact(&mut response_buffer).unwrap();
                            self.pending_response.append(&mut response_buffer);
                            self.pending_response_offset += number_of_bytes_to_read;
                            if self.pending_response_offset == self.pending_response_len {
                                self.received_responses
                                    .insert(self.pending_command_id, self.pending_response.clone());
                                self.pending_response.clear();
                                self.pending_response_offset = 0;
                                self.pending_command_id = 0;
                                self.pending_response_len = 0;
                            }
                        }
                    }
                    if event.is_writable() {
                        eprintln!("There should not be any writable events")
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    pub fn get_udp_port(&self) -> u16 {
        self.udp_port
    }
}

fn serialize<T: Serialize>(s: &T) -> Vec<u8> {
    bincode::serialize(s).unwrap()
}

fn deserialize<T: DeserializeOwned + 'static>(encoded: &[u8]) -> T {
    bincode::deserialize(encoded).unwrap()
}

#[cfg(test)]
mod tests {
    use crate::network::{deserialize, serialize};
    use crate::robot::types::PandaStateIntern;

    #[test]
    fn can_serialize_and_deserialize() {
        let state = PandaStateIntern::dummy();
        let state2: PandaStateIntern = deserialize(&serialize(&state));
        assert_eq!(state, state2);
    }
}
