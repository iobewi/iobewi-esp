//! ESP capabilities supplied to the portable IOBEWI OTA service.
//! The ConfigSpace implementation shares the same physical flash owner as
//! partition reads, so each OTA operation drops its flash lock before a
//! metadata load or commit.

extern crate alloc;

use alloc::vec::Vec;

use iobewi_config_space::ConfigSpace;
use iobewi_esp_config_space::NvsConfigBackend;
use iobewi_esp_flash::SharedFlash;
use iobewi_ota::bootstrap::BootstrapStore as BootstrapStoreContract;
use iobewi_ota::metadata::MetadataStore;
use iobewi_ota::service::{BootActivation, TargetSelection};
use iobewi_ota::service::upload::{UploadStats, UploadWriter};
use iobewi_ota::service::boot::BootOps;
use iobewi_ota::{Committed, Error, WriteSession};
use iobewi_ota::BackendOutcome;

use crate::{AppPartition, AppSlot, FlashWriteError, shared_flash};

pub type OtaConfigSpace = ConfigSpace<NvsConfigBackend>;
pub type BootstrapConfigSpace = ConfigSpace<NvsConfigBackend>;

pub struct BootstrapStore<'a>(pub &'a BootstrapConfigSpace);

impl BootstrapStoreContract for BootstrapStore<'_> {
    type Error = ();

    async fn load_raw(&self) -> Result<Option<Vec<u8>>, Self::Error> {
        self.0.load().await
            .map(|snapshot| snapshot.map(|snapshot| snapshot.data))
            .map_err(|_| ())
    }

    async fn commit_raw(&self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.commit(bytes).await.map(|_| ()).map_err(|_| ())
    }
}


pub struct OtaStore<'a>(pub &'a OtaConfigSpace);

impl MetadataStore for OtaStore<'_> {
    type Error = ();

    async fn load_raw(&self) -> Result<Option<Vec<u8>>, Self::Error> {
        self.0.load().await
            .map(|snapshot| snapshot.map(|snapshot| snapshot.data))
            .map_err(|_| ())
    }

    async fn commit_raw(&self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.commit(bytes).await.map(|_| ()).map_err(|_| ())
    }
}

pub struct EspBoot<'a>(pub &'a SharedFlash);

impl TargetSelection for EspBoot<'_> {
    async fn write_target(&self) -> Result<(&'static str, u64), ()> {
        let partition = shared_flash::write_target(self.0).await.map_err(|_| ())?;
        Ok((partition.slot.as_str(), partition.size as u64))
    }
}

impl BootActivation for EspBoot<'_> {
    fn valid_target(&self, target: &str) -> bool {
        AppSlot::from_name(target).is_some()
    }

    async fn activate(&self, target: &str) -> Result<(), ()> {
        let slot = AppSlot::from_name(target).ok_or(())?;
        shared_flash::activate(self.0, slot).await.map_err(|_| ())
    }
}

pub struct EspBootRuntime<'a> {
    pub flash: &'a SharedFlash,
    pub nvs: &'a NvsConfigBackend,
}

impl BootOps for EspBootRuntime<'_> {
    async fn image_outcome(&self) -> BackendOutcome {
        shared_flash::image_outcome(self.flash).await
    }

    async fn booted_slot(&self) -> alloc::string::String {
        alloc::string::String::from(shared_flash::active_slot(self.flash).await)
    }

    async fn confirm(&self) -> bool { shared_flash::confirm(self.flash).await.is_ok() }
    async fn reject(&self) -> bool { shared_flash::reject(self.flash).await.is_ok() }
    async fn self_check(&self) -> bool { self.nvs.self_check().await }
    fn watchdog_feed(&self) { iobewi_esp_watchdog::feed(); }
    fn watchdog_disable(&self) { iobewi_esp_watchdog::disable(); }
}

pub fn arm_watchdog_ms(deadline: u64) { iobewi_esp_watchdog::arm_ms(deadline); }

pub fn reset_now() -> ! { esp_hal::system::software_reset() }

pub async fn reset_for_rollback() -> ! {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(200)).await;
    reset_now()
}

pub async fn reject_and_reset(flash: &SharedFlash) -> ! {
    let _ = shared_flash::reject(flash).await;
    reset_for_rollback().await
}

/// Holds one ESP partition writer while IOBEWI OTA owns the session state.
pub struct EspUploadWriter {
    flash: &'static SharedFlash,
    inner: shared_flash::ArtifactWriter,
}

impl EspUploadWriter {
    pub fn new(flash: &'static SharedFlash, target: AppPartition) -> Self {
        Self { flash, inner: shared_flash::ArtifactWriter::new(target) }
    }
}

impl UploadWriter for EspUploadWriter {
    type Error = FlashWriteError;

    fn slot(&self) -> &'static str { self.inner.slot().as_str() }

    fn stats(&self) -> UploadStats {
        UploadStats {
            sectors_flushed: self.inner.sectors_flushed(),
            erase_batches: self.inner.erase_batches(),
            erase_batch_kib: (shared_flash::erase_batch_size() / 1024) as usize,
        }
    }

    async fn append(&mut self, session: &mut WriteSession, data: &[u8]) -> bool {
        self.inner.append(self.flash, session, data).await
    }

    async fn finish(&mut self, session: WriteSession) -> Result<Committed, Error<Self::Error>> {
        self.inner.finish(self.flash, session).await
    }
}
