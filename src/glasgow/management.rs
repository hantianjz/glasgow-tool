//! Pinned Glasgow API-9 management and embedded-resource validation.

use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use nusb::io::{EndpointRead, EndpointWrite};
use nusb::transfer::{Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Device, Interface, MaybeFuture};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cli::AppError;
use crate::resources::{GlasgowResource, for_revision};

use super::device::{API_LEVEL, GLASGOW_PID, GLASGOW_VID, GlasgowDeviceInfo, list_devices};

const REQUEST_FPGA_LOAD_CFG: u8 = 0x20;
const REQUEST_FPGA_STATUS: u8 = 0x22;
const REQUEST_FPGA_SET_REG: u8 = 0x28;
const REQUEST_FPGA_GET_REG: u8 = 0x29;
const REQUEST_SET_VSUPPLY: u8 = 0x30;
const REQUEST_SET_PULLS: u8 = 0x3a;
const RESULT_ACK: u8 = 0x00;
const RESULT_WAIT: u8 = 0x01;
const CYPRESS_RAM: u8 = 0xa0;
const CYPRESS_CPUCS: u16 = 0xe600;
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);
const FPGA_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize)]
pub struct Register {
    pub address: u8,
    pub width_bits: u8,
    pub storage_bytes: u8,
    pub endian: String,
    pub access: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PipeEndpoint {
    pub interface: u8,
    pub alternate_setting: u8,
    pub endpoint: u8,
    pub max_packet: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FirmwareSegment {
    pub address: u16,
    pub data_hex: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResourceManifest {
    pub schema_version: u8,
    pub upstream_commit: String,
    pub revision: String,
    pub profile: Profile,
    pub bitstream: Bitstream,
    pub registers: Registers,
    pub pipe: Pipe,
    pub firmware: Firmware,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub port: String,
    pub voltage: f64,
    pub rx: String,
    pub tx: String,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow_control: String,
    pub inverted: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Bitstream {
    pub file: String,
    pub id: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Registers {
    pub baud_divisor: Register,
    pub rx_errors: Register,
    pub rx_overflow: Register,
    pub tx_state: Register,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Pipe {
    pub rx: PipeEndpoint,
    pub tx: PipeEndpoint,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Firmware {
    pub api_level: u8,
    pub source_sha256: String,
    pub segments: Vec<FirmwareSegment>,
}

pub struct ValidatedResource {
    pub embedded: &'static GlasgowResource,
    pub manifest: ResourceManifest,
    pub bitstream_id: [u8; 8],
}

fn decode_hex(value: &str) -> Result<Vec<u8>, AppError> {
    if !value.len().is_multiple_of(2) {
        return Err(AppError::Protocol(
            "resource contains odd-length hex".to_owned(),
        ));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| AppError::Protocol("resource contains invalid hex".to_owned()))?;
            u8::from_str_radix(text, 16)
                .map_err(|_| AppError::Protocol("resource contains invalid hex".to_owned()))
        })
        .collect()
}

pub fn validate_resource(revision: &str) -> Result<ValidatedResource, AppError> {
    let embedded = for_revision(revision).ok_or_else(|| {
        AppError::Protocol(format!(
            "Glasgow revision {revision} is unsupported; expected C0, C1, C2, or C3"
        ))
    })?;
    let (manifest, bitstream_id) =
        validate_resource_data(revision, embedded.manifest, embedded.bitstream)?;
    Ok(ValidatedResource {
        embedded,
        manifest,
        bitstream_id,
    })
}

#[doc(hidden)]
pub fn validate_resource_data(
    revision: &str,
    manifest_bytes: &[u8],
    bitstream: &[u8],
) -> Result<(ResourceManifest, [u8; 8]), AppError> {
    let manifest: ResourceManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|error| AppError::Protocol(format!("invalid embedded manifest: {error}")))?;
    let profile_valid = manifest.profile.port == "A"
        && manifest.profile.voltage == 3.3
        && manifest.profile.rx == "A0"
        && manifest.profile.tx == "A1"
        && manifest.profile.data_bits == 8
        && manifest.profile.parity == "none"
        && manifest.profile.stop_bits == 1
        && manifest.profile.flow_control == "none"
        && !manifest.profile.inverted;
    if manifest.schema_version != 1
        || manifest.revision != revision
        || manifest.firmware.api_level != API_LEVEL
        || !profile_valid
    {
        return Err(AppError::Protocol(
            "embedded Glasgow resource schema, revision, firmware, or profile mismatch".to_owned(),
        ));
    }
    let digest = format!("{:x}", Sha256::digest(bitstream));
    if digest != manifest.bitstream.sha256 || manifest.bitstream.file != format!("{revision}.bit") {
        return Err(AppError::Protocol(
            "embedded Glasgow bitstream digest mismatch".to_owned(),
        ));
    }
    let id = decode_hex(&manifest.bitstream.id)?;
    let bitstream_id: [u8; 8] = id
        .try_into()
        .map_err(|_| AppError::Protocol("Glasgow bitstream ID is not eight bytes".to_owned()))?;
    for register in [
        &manifest.registers.baud_divisor,
        &manifest.registers.rx_errors,
        &manifest.registers.rx_overflow,
        &manifest.registers.tx_state,
    ] {
        if register.address > 0x7f
            || register.storage_bytes == 0
            || register.storage_bytes > 4
            || register.width_bits == 0
            || register.width_bits > 32
            || register.endian != "little"
            || !matches!(register.access.as_str(), "ro" | "rw")
        {
            return Err(AppError::Protocol(
                "embedded Glasgow register metadata mismatch".to_owned(),
            ));
        }
    }
    if manifest.pipe.rx.endpoint & 0x80 == 0
        || manifest.pipe.tx.endpoint & 0x80 != 0
        || manifest.pipe.rx.max_packet != 512
        || manifest.pipe.tx.max_packet != 512
    {
        return Err(AppError::Protocol(
            "embedded Glasgow pipe metadata mismatch".to_owned(),
        ));
    }
    Ok((manifest, bitstream_id))
}

pub struct Management {
    _interface: Interface,
    reader: EndpointRead<Bulk>,
    writer: EndpointWrite<Bulk>,
    serial: u8,
}

impl Management {
    pub fn claim(device: &Device) -> Result<Self, AppError> {
        let interface = device.claim_interface(0).wait().map_err(|error| {
            AppError::Access(format!(
                "cannot claim Glasgow management interface: {error}"
            ))
        })?;
        interface.set_alt_setting(1).wait().map_err(|error| {
            AppError::Access(format!(
                "cannot enable Glasgow management interface: {error}"
            ))
        })?;
        let reader = EndpointRead::new(
            interface.endpoint::<Bulk, In>(0x81).map_err(|error| {
                AppError::Protocol(format!("Glasgow management IN endpoint mismatch: {error}"))
            })?,
            64,
        )
        .with_num_transfers(4)
        .with_read_timeout(COMMAND_TIMEOUT);
        let writer = EndpointWrite::new(
            interface.endpoint::<Bulk, Out>(0x01).map_err(|error| {
                AppError::Protocol(format!("Glasgow management OUT endpoint mismatch: {error}"))
            })?,
            64,
        )
        .with_num_transfers(4)
        .with_write_timeout(COMMAND_TIMEOUT);
        Ok(Self {
            _interface: interface,
            reader,
            writer,
            serial: 1,
        })
    }

    pub fn command(&mut self, payload: &[u8]) -> Result<Vec<u8>, AppError> {
        if payload.len() > 63 {
            return Err(AppError::Protocol(
                "Glasgow management command exceeds 63-byte payload".to_owned(),
            ));
        }
        let serial = self.serial;
        self.serial = if serial == u8::MAX { 1 } else { serial + 1 };
        let mut packet = Vec::with_capacity(payload.len() + 1);
        packet.push(serial);
        packet.extend_from_slice(payload);
        self.writer
            .write_all(&packet)
            .and_then(|()| self.writer.flush())
            .map_err(|error| {
                AppError::Access(format!("Glasgow management write failed: {error}"))
            })?;
        let mut response = [0_u8; 64];
        let length = self.reader.read(&mut response).map_err(|error| {
            AppError::Access(format!("Glasgow management response failed: {error}"))
        })?;
        if length == 0 || response[0] != serial {
            return Err(AppError::Protocol(
                "Glasgow management response serial mismatch".to_owned(),
            ));
        }
        Ok(response[1..length].to_vec())
    }

    fn expect_ack(&mut self, payload: &[u8], operation: &str) -> Result<(), AppError> {
        let response = self.command(payload)?;
        if response == [RESULT_ACK] {
            Ok(())
        } else {
            Err(AppError::Protocol(format!(
                "Glasgow {operation} returned {response:02x?}"
            )))
        }
    }

    pub fn fpga_status(&mut self) -> Result<Option<[u8; 8]>, AppError> {
        let response = self.command(&[REQUEST_FPGA_STATUS])?;
        if response.len() != 13 || response[0] != RESULT_ACK {
            return Err(AppError::Protocol(
                "invalid Glasgow FPGA status response".to_owned(),
            ));
        }
        let id: [u8; 8] = response[5..13].try_into().expect("checked response length");
        Ok((id != [0; 8]).then_some(id))
    }

    pub fn finish_fpga_load(
        &mut self,
        length: usize,
        bitstream_id: [u8; 8],
    ) -> Result<(), AppError> {
        let length = u32::try_from(length)
            .map_err(|_| AppError::Protocol("Glasgow bitstream is too large".to_owned()))?;
        let mut payload = Vec::with_capacity(13);
        payload.push(REQUEST_FPGA_LOAD_CFG);
        payload.extend_from_slice(&length.to_le_bytes());
        payload.extend_from_slice(&bitstream_id);
        let deadline = Instant::now() + FPGA_TIMEOUT;
        loop {
            match self.command(&payload)?.as_slice() {
                [RESULT_ACK] => return Ok(()),
                [RESULT_WAIT] if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                response => {
                    return Err(AppError::Protocol(format!(
                        "Glasgow FPGA configuration failed with {response:02x?}"
                    )));
                }
            }
        }
    }

    pub fn write_register(&mut self, register: &Register, value: u32) -> Result<(), AppError> {
        let width = usize::from(register.storage_bytes);
        let bytes = value.to_be_bytes();
        let mut payload = Vec::with_capacity(width + 2);
        payload.extend_from_slice(&[REQUEST_FPGA_SET_REG, register.address]);
        payload.extend_from_slice(&bytes[bytes.len() - width..]);
        self.expect_ack(&payload, "register write")
    }

    pub fn read_register(&mut self, register: &Register) -> Result<u32, AppError> {
        let response = self.command(&[
            REQUEST_FPGA_GET_REG,
            register.address,
            register.storage_bytes,
        ])?;
        if response.len() != usize::from(register.storage_bytes) + 1 || response[0] != RESULT_ACK {
            return Err(AppError::Protocol(
                "invalid Glasgow register response".to_owned(),
            ));
        }
        let mut bytes = [0_u8; 4];
        let width = usize::from(register.storage_bytes);
        bytes[..width].copy_from_slice(&response[1..]);
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn set_fixed_profile(&mut self, enable: bool) -> Result<(), AppError> {
        let millivolts = if enable { 3300_u16 } else { 0 };
        let mut voltage = vec![REQUEST_SET_VSUPPLY, 0x01];
        for _ in 0..4 {
            voltage.extend_from_slice(&millivolts.to_le_bytes());
        }
        self.expect_ack(&voltage, "port-A voltage configuration")?;
        let mut pulls = vec![REQUEST_SET_PULLS, 0x01];
        pulls.extend_from_slice(&[0, u8::from(enable), 0, 0, 0, 0, 0, 0]);
        self.expect_ack(&pulls, "port-A pull configuration")
    }
}

pub fn upload_fx2_firmware(
    selected: &GlasgowDeviceInfo,
    resource: &ValidatedResource,
) -> Result<GlasgowDeviceInfo, AppError> {
    if selected.native.vendor_id() != GLASGOW_VID || selected.native.product_id() != GLASGOW_PID {
        return Err(AppError::Protocol(
            "refusing firmware upload to a non-Glasgow USB identity".to_owned(),
        ));
    }
    let device = selected.native.open().wait().map_err(|error| {
        AppError::Access(format!("cannot open Glasgow for firmware upload: {error}"))
    })?;
    let interface = device.claim_interface(0).wait().map_err(|error| {
        AppError::Access(format!("cannot claim Glasgow for firmware upload: {error}"))
    })?;
    let ram_write = |address: u16, data: &[u8]| -> Result<(), AppError> {
        interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: CYPRESS_RAM,
                    value: address,
                    index: 0,
                    data,
                },
                COMMAND_TIMEOUT,
            )
            .wait()
            .map_err(|error| AppError::Access(format!("Cypress RAM upload failed: {error}")))?;
        Ok(())
    };
    ram_write(CYPRESS_CPUCS, &[1])?;
    for segment in &resource.manifest.firmware.segments {
        let data = decode_hex(&segment.data_hex)?;
        for (offset, chunk) in data.chunks(4096).enumerate() {
            let offset = u16::try_from(offset * 4096)
                .map_err(|_| AppError::Protocol("FX2 firmware segment is too large".to_owned()))?;
            ram_write(segment.address.wrapping_add(offset), chunk)?;
        }
    }
    ram_write(CYPRESS_CPUCS, &[0])?;
    drop(interface);
    drop(device);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(device) = list_devices()?.into_iter().find(|candidate| {
            candidate.serial == selected.serial && candidate.path == selected.path
        }) {
            return Ok(device);
        }
        if Instant::now() >= deadline {
            return Err(AppError::Timeout(
                "Glasgow did not re-enumerate after FX2 firmware upload".to_owned(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}
