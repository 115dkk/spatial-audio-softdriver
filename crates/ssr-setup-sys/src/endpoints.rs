//! Our render endpoints, found the way `tools/ci/endpoints.ps1` (`Enumerate`)
//! and `ssr-sys::host` find them: every render endpoint in every state, mapped
//! to our endpoint number through the KS filter its topology connects to
//! (`ssr_abi::ks_path::map_filter`). The walk is repeated here rather than
//! shared because this crate must not depend on `ssr-sys` (ADR 0032).

use ssr_abi::ks_path::map_filter;
use windows::Win32::Media::Audio::{DEVICE_STATE_ACTIVE, IMMDevice};

use crate::tasks::Failure;
use crate::win;

/// One of our endpoints, with the device to write its policy through.
#[derive(Debug)]
pub(crate) struct OurEndpoint {
    pub(crate) endpoint: u16,
    pub(crate) id: String,
    pub(crate) state: u32,
    pub(crate) device: IMMDevice,
}

impl OurEndpoint {
    pub(crate) fn is_active(&self) -> bool {
        self.state == DEVICE_STATE_ACTIVE.0
    }
}

/// The endpoints whose KS filter belongs to one of `owners` (device instance
/// ids). An endpoint whose topology cannot be read is left out, as the
/// script leaves it out, and its number is never guessed from its name. Two
/// numbers on one device fail the whole listing, as they do in the script.
pub(crate) fn our_endpoints(owners: &[String]) -> Result<Vec<OurEndpoint>, Failure> {
    let enumerator = win::device_enumerator()?;
    let mut found = Vec::new();
    'devices: for device in win::render_endpoints(&enumerator)? {
        let Ok(connectors) = win::connectors(&device) else {
            continue;
        };
        let mut endpoint = None;
        for connector in &connectors {
            let path = match win::connected_device_id(connector) {
                Ok(Some(path)) => path,
                Ok(None) => continue,
                Err(_) => continue 'devices,
            };
            let Some(mapped) = map_filter(&path, owners) else {
                continue;
            };
            match endpoint {
                Some(previous) if previous != mapped => {
                    return Err(Failure::new(format!(
                        "ambiguous endpoint topology: one device connects to endpoints {previous} and {mapped}"
                    )));
                }
                _ => endpoint = Some(mapped),
            }
        }
        let Some(endpoint) = endpoint else {
            continue;
        };
        let (Ok(id), Ok(state)) = (win::endpoint_id(&device), win::endpoint_state(&device)) else {
            continue;
        };
        found.push(OurEndpoint {
            endpoint,
            id,
            state,
            device,
        });
    }
    Ok(found)
}
