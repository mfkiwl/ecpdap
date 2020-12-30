//! This module implements ECP5-specific functionality on top of a JTAG TAP.

use std::convert::TryFrom;
use std::fmt;
use num_enum::{FromPrimitive, TryFromPrimitive};
use indicatif::{ProgressBar, ProgressStyle};
use crate::jtag::{IDCODE, JTAGTAP, Error as JTAGError};
use crate::bitvec::{byte_to_bits, bytes_to_bits, bits_to_bytes, drain_u32, Error as BitvecError};
use crate::flash::FlashAccess;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("ECP5 status register in incorrect state.")]
    BadStatus,
    #[error("Cannot access flash memory unless the ECP5 is the only TAP in the JTAG chain.")]
    NotOnlyTAP,
    #[error("JTAG error")]
    JTAG(#[from] JTAGError),
    #[error("Bitvec error")]
    Bitvec(#[from] BitvecError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// IDCODEs for all ECP5 device types.
#[derive(Copy, Clone, Debug, Eq, PartialEq, TryFromPrimitive)]
#[allow(non_camel_case_types)]
#[repr(u32)]
pub enum ECP5IDCODE {
    LFE5U_12 = 0x21111043,
    LFE5U_25 = 0x41111043,
    LFE5U_45 = 0x41112043,
    LFE5U_85 = 0x41113043,
    LFE5UM_25 = 0x01111043,
    LFE5UM_45 = 0x01112043,
    LFE5UM_85 = 0x01113043,
    LFE5UM5G_25 = 0x81111043,
    LFE5UM5G_45 = 0x81112043,
    LFE5UM5G_85 = 0x81113043,
}

impl ECP5IDCODE {
    pub fn try_from_idcode(idcode: &IDCODE) -> Option<Self> {
        Self::try_from(idcode.0).ok()
    }

    pub fn name(&self) -> &'static str {
        match self {
            ECP5IDCODE::LFE5U_12 => "LFE5U-12",
            ECP5IDCODE::LFE5U_25 => "LFE5U-25",
            ECP5IDCODE::LFE5U_45 => "LFE5U-45",
            ECP5IDCODE::LFE5U_85 => "LFE5U-85",
            ECP5IDCODE::LFE5UM_25 => "LFE5UM-25",
            ECP5IDCODE::LFE5UM_45 => "LFE5UM-45",
            ECP5IDCODE::LFE5UM_85 => "LFE5UM-85",
            ECP5IDCODE::LFE5UM5G_25 => "LFE5UM5G-25",
            ECP5IDCODE::LFE5UM5G_45 => "LFE5UM5G-45",
            ECP5IDCODE::LFE5UM5G_85 => "LFE5UM5G-85",
        }
    }
}

/// All known ECP5 JTAG instructions.
#[derive(Copy, Clone, Debug)]
#[allow(unused, non_camel_case_types)]
#[repr(u8)]
enum Command {
    ISC_NOOP = 0xFF,
    READ_ID  = 0xE0,
    USERCODE = 0xC0,
    LSC_READ_STATUS = 0x3C,
    LSB_CHECK_BUSY = 0xF0,
    LSC_REFRESH = 0x79,
    ISC_ENABLE = 0xC6,
    ISC_ENABLE_X = 0x74,
    ISC_DISABLE = 0x26,
    ISC_PROGRAM_USERCODE = 0xC2,
    ISC_ERASE = 0x0E,
    ISC_PROGRAM_DONE = 0x5E,
    ISC_PROGRAM_SECURITY = 0xCE,
    LSC_INIT_ADDRESS = 0x46,
    LSC_WRITE_ADDRESS = 0xB4,
    LSC_BITSTREAM_BURST = 0x7A,
    LSC_PROG_INCR_RTI = 0x82,
    LSC_PROG_INCR_ENC = 0xB6,
    LSC_PROG_INCR_CMP = 0xB8,
    LSC_PROG_INCR_CNE = 0xBA,
    LSC_VERIFY_INCR_RTI = 0x6A,
    LSC_PROG_CTRL0 = 0x22,
    LSC_READ_CTRL0 = 0x20,
    LSC_RESET_CRC = 0x3B,
    LSC_READ_CRC = 0x60,
    LSC_PROG_SED_CRC = 0xA2,
    LSC_READ_SED_CRC = 0xA4,
    LSC_PROG_PASSWORD = 0xF1,
    LSC_READ_PASSWORD = 0xF2,
    LSC_SHIFT_PASSWORD = 0xBC,
    LSC_PROG_CIPHER_KEY = 0xF3,
    LSC_READ_CIPHER_KEY = 0xF4,
    LSC_PROG_FEATURE = 0xE4,
    LSC_READ_FEATURE = 0xE7,
    LSC_PROG_FEABITS = 0xF8,
    LSC_READ_FEABITS = 0xFB,
    LSC_PROG_OTP = 0xF9,
    LSC_READ_OTP = 0xFA,
    LSC_BACKGROUND_SPI = 0x3A,
}

impl Command {
    pub fn bits(&self) -> Vec<bool> {
        byte_to_bits(*self as u8)
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[allow(unused, non_camel_case_types)]
#[repr(u8)]
pub enum BSEError {
    #[num_enum(default)]
    NoError = 0,
    IDError = 1,
    CMDError = 2,
    CRCError = 3,
    PRMBError = 4,
    ABRTError = 5,
    OVFLError = 6,
    SDMError = 7,
}

#[derive(Copy, Clone, Debug)]
#[allow(unused, non_camel_case_types)]
#[repr(u8)]
pub enum ConfigTarget {
    SRAM = 0,
    eFuse = 1,
    Unknown = 0xF,
}

/// ECP5 Status register.
#[derive(Copy, Clone)]
pub struct Status(u32);

impl Status {
    pub fn new(word: u32) -> Self {
        Self(word)
    }

    pub fn transparent(&self) -> bool       { self.bit(0) }
    pub fn jtag_active(&self) -> bool       { self.bit(4) }
    pub fn pwd_protection(&self) -> bool    { self.bit(5) }
    pub fn decrypt_enable(&self) -> bool    { self.bit(7) }
    pub fn done(&self) -> bool              { self.bit(8) }
    pub fn isc_enable(&self) -> bool        { self.bit(9) }
    pub fn write_enable(&self) -> bool      { self.bit(10) }
    pub fn read_enable(&self) -> bool       { self.bit(11) }
    pub fn busy(&self) -> bool              { self.bit(12) }
    pub fn fail(&self) -> bool              { self.bit(13) }
    pub fn feature_otp(&self) -> bool       { self.bit(14) }
    pub fn decrypt_only(&self) -> bool      { self.bit(15) }
    pub fn pwd_enable(&self) -> bool        { self.bit(16) }
    pub fn encrypt_preamble(&self) -> bool  { self.bit(20) }
    pub fn standard_preamble(&self) -> bool { self.bit(21) }
    pub fn spim_fail(&self) -> bool         { self.bit(22) }
    pub fn execution_error(&self) -> bool   { self.bit(26) }
    pub fn id_error(&self) -> bool          { self.bit(27) }
    pub fn invalid_command(&self) -> bool   { self.bit(28) }
    pub fn sed_error(&self) -> bool         { self.bit(29) }
    pub fn bypass_mode(&self) -> bool       { self.bit(30) }
    pub fn flow_through_mode(&self) -> bool { self.bit(31) }

    pub fn config_target(&self) -> ConfigTarget {
        match (self.0 >> 1) & 0b111 {
            0 => ConfigTarget::SRAM,
            1 => ConfigTarget::eFuse,
            _ => ConfigTarget::Unknown,
        }
    }

    pub fn bse_error(&self) -> BSEError {
        BSEError::from(((self.0 >> 23) & 0b111) as u8)
    }

    fn bit(&self, offset: usize) -> bool {
        (self.0 >> offset) & 1 == 1
    }
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "ECP5 Status: {:08X}
             Transparent: {}
             Config target: {:?}
             JTAG active: {}
             PWD protection: {}
             Decrypt enable: {}
             DONE: {}
             ISC enable: {}
             Write enable: {}
             Read enable: {}
             Busy: {}
             Fail: {}
             Feature OTP: {}
             Decrypt only: {}
             PWD enable: {}
             Encrypt preamble: {}
             Standard preamble: {}
             SPIm fail 1: {}
             BSE error: {:?}
             Execution error: {}
             ID error: {}
             Invalid command: {}
             SED error: {}
             Bypass mode: {}
             Flow-through mode: {}",
            self.0, self.transparent(), self.config_target(),
            self.jtag_active(), self.pwd_protection(), self.decrypt_enable(),
            self.done(), self.isc_enable(), self.write_enable(),
            self.read_enable(), self.busy(), self.fail(), self.feature_otp(),
            self.decrypt_only(), self.pwd_enable(), self.encrypt_preamble(),
            self.standard_preamble(), self.spim_fail(), self.bse_error(),
            self.execution_error(), self.id_error(), self.invalid_command(),
            self.sed_error(), self.bypass_mode(), self.flow_through_mode()))
    }
}

/// ECP5 FPGA manager
pub struct ECP5 {
    tap: JTAGTAP,
}

impl ECP5 {
    pub fn new(tap: JTAGTAP) -> Self {
        ECP5 { tap }
    }

    /// Read current status register content.
    pub fn status(&mut self) -> Result<Status> {
        log::trace!("Reading status register");
        self.command(Command::LSC_READ_STATUS)?;
        let data = self.tap.read_dr(32)?;
        let (status, _) = drain_u32(&data)?;
        Ok(Status::new(status))
    }

    /// Program the ECP5 configuration SRAM.
    ///
    /// The ECP5 will be reset and start configuration after programming completion.
    pub fn program(&mut self, data: &[u8]) -> Result<()> {
        // Enable configuration interface.
        self.command(Command::ISC_ENABLE)?;
        self.tap.run_test_idle(50)?;

        self.check_ready_to_program()?;

        self.command(Command::LSC_BITSTREAM_BURST)?;

        // Create a progress bar for loading.
        let pb = ProgressBar::new(data.len() as u64).with_style(ProgressStyle::default_bar()
            .template(" {msg} [{bar:40}] {bytes}/{total_bytes} ({bytes_per_sec}; {eta})")
            .progress_chars("=> "));
        pb.set_message("Programming");

        // Load in entire bitstream.
        // We have to bit-reverse each byte of bitstream for the ECP5.
        let data: Vec<u8> = data.iter().map(|x| x.reverse_bits()).collect();
        let bits = bytes_to_bits(&data, data.len() * 8)?;

        // Write in ten-packet chunks, to give the progress bar something to do.
        for chunk in bits.chunks(self.tap.max_tdi_bits() * 10) {
            self.tap.write_dr_raw(&chunk)?;
            pb.inc(chunk.len() as u64 / 8);
        }

        // Return to Run-Test/Idle to complete programming.
        self.tap.run_test_idle(1)?;

        // Disable configuration interface.
        self.command(Command::ISC_DISABLE)?;
        self.tap.run_test_idle(50)?;

        self.check_programmed_ok()?;

        self.tap.run_test_idle(0)?;

        Ok(())
    }

    /// Place ECP5 into flash pass-through mode.
    /// The current SRAM contents are cleared.
    pub fn into_flash(mut self) -> Result<ECP5Flash> {
        if self.tap.n_taps() > 1 {
            log::error!(
                "SPI flash access is not possible with more than one TAP in the JTAG chain.");
            return Err(Error::NotOnlyTAP);
        }

        // Enable ISC to erase SRAM content, required before SPI passthrough is used.
        self.tap.run_test_idle(0)?;
        self.command(Command::ISC_ENABLE)?;
        self.tap.run_test_idle(50)?;
        self.command(Command::ISC_ERASE)?;
        self.tap.run_test_idle(50)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        self.command(Command::ISC_DISABLE)?;
        self.tap.run_test_idle(50)?;

        // Enable SPI mode and shift in magic numbers.
        self.command(Command::LSC_BACKGROUND_SPI)?;
        self.tap.write_dr(&bytes_to_bits(&[0xFE, 0x68], 16)?)?;
        self.tap.run_test_idle(50)?;

        Ok(ECP5Flash::new(self))
    }

    /// Reads current status and checks it seems suitable for SRAM programming.
    fn check_ready_to_program(&mut self) -> Result<()> {
        let status = self.status()?;
        log::debug!("Checking ECP5 status before programming ({:08X})", status.0);
        match status.config_target() {
            ConfigTarget::SRAM => (),
            target => {
                log::error!("Incorrect configuration target: {:?}", target);
                return Err(Error::BadStatus);
            }
        }
        match status.bse_error() {
            BSEError::NoError => (),
            error => {
                log::warn!("BSE error present: {:?}", error);
            }
        }
        if !status.jtag_active() {
            log::error!("JTAG reported as not active");
            return Err(Error::BadStatus);
        }
        if !status.isc_enable() {
            log::error!("ISC reported as not enabled");
            return Err(Error::BadStatus);
        }
        if !status.write_enable() {
            log::error!("Write-enable not set");
            return Err(Error::BadStatus);
        }
        if status.busy() {
            log::error!("BUSY flag currently set");
            return Err(Error::BadStatus);
        }
        Ok(())
    }

    /// Reads current status and checks no errors reported after SRAM programming.
    fn check_programmed_ok(&mut self) -> Result<()> {
        let status = self.status()?;
        log::debug!("Checking ECP5 status after programming ({:08X})", status.0);
        match status.bse_error() {
            BSEError::NoError => (),
            error => {
                log::error!("BSE error present: {:?}", error);
                return Err(Error::BadStatus);
            }
        }
        if !status.done() {
            log::error!("DONE flag not set");
            return Err(Error::BadStatus);
        }
        if status.fail() {
            log::error!("FAIL flag set");
            return Err(Error::BadStatus);
        }
        Ok(())
    }

    /// Load a command into the IR.
    fn command(&mut self, command: Command) -> Result<()> {
        log::trace!("Loading ECP5 command {:?}", command);
        Ok(self.tap.write_ir(&command.bits())?)
    }
}

/// Access to attached SPI flash on an ECP5.
pub struct ECP5Flash {
    ecp5: ECP5,
}

impl ECP5Flash {
    fn new(ecp5: ECP5) -> Self {
        ECP5Flash { ecp5 }
    }

    pub fn release(self) -> ECP5 {
        self.ecp5
    }
}

impl FlashAccess for ECP5Flash {
    fn write(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let data: Vec<u8> = data.iter().map(|x| x.reverse_bits()).collect();
        let bits = bytes_to_bits(&data, data.len() * 8)?;
        self.ecp5.tap.run_test_idle(0)?;
        self.ecp5.tap.write_dr(&bits)?;
        self.ecp5.tap.run_test_idle(0)?;
        Ok(())
    }

    fn exchange(&mut self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let data: Vec<u8> = data.iter().map(|x| x.reverse_bits()).collect();
        let bits = bytes_to_bits(&data, data.len() * 8)?;
        self.ecp5.tap.run_test_idle(0)?;
        let result = self.ecp5.tap.exchange_dr(&bits)?;
        self.ecp5.tap.run_test_idle(0)?;
        let result = bits_to_bytes(&result);
        let result: Vec<u8> = result.iter().map(|x| x.reverse_bits()).collect();
        Ok(result)
    }
}
