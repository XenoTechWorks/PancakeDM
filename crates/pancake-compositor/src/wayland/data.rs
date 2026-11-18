//! wl_data_device_manager: copy and paste (the clipboard "selection").
//! The app that copied keeps the data; when another app pastes, we hand
//! it a pipe and the copying app writes into it. Drag and drop comes later.

use super::{same_client, State};
use std::os::fd::AsFd;
use std::sync::Mutex;
use wayland_server::backend::ClientId;
use wayland_server::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{self, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

#[derive(Default)]
pub struct SourceData {
    mimes: Mutex<Vec<String>>,
}

pub struct OfferData {
    source: WlDataSource,
}

impl State {
    /// Tell the focused app what's on the clipboard.
    pub(super) fn send_selection(&mut self) {
        let Some(focus) = self.kb_focus.clone() else { return };
        let Some(dh) = self.dh.clone() else { return };
        self.data_devices.retain(|d| d.is_alive());
        for dev in self.data_devices.iter().filter(|d| same_client(*d, &focus)) {
            let Some(src) = self.selection.as_ref().filter(|s| s.is_alive()) else {
                dev.selection(None);
                continue;
            };
            let Ok(client) = dh.get_client(dev.id()) else { continue };
            let Ok(offer) = client.create_resource::<WlDataOffer, OfferData, State>(&dh, dev.version(), OfferData { source: src.clone() }) else {
                continue;
            };
            dev.data_offer(&offer);
            if let Some(data) = src.data::<SourceData>() {
                for m in data.mimes.lock().unwrap().iter() {
                    offer.offer(m.clone());
                }
            }
            dev.selection(Some(&offer));
        }
    }
}

impl GlobalDispatch<WlDataDeviceManager, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WlDataDeviceManager>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn request(state: &mut Self, _: &Client, _: &WlDataDeviceManager, request: wl_data_device_manager::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                init.init(id, SourceData::default());
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                let dev = init.init(id, ());
                state.data_devices.push(dev);
                state.send_selection();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, SourceData> for State {
    fn request(_: &mut Self, _: &Client, _: &WlDataSource, request: wl_data_source::Request, data: &SourceData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wl_data_source::Request::Offer { mime_type } = request {
            data.mimes.lock().unwrap().push(mime_type);
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, source: &WlDataSource, _: &SourceData) {
        if state.selection.as_ref() == Some(source) {
            state.selection = None;
            state.send_selection();
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn request(state: &mut Self, _: &Client, _: &WlDataDevice, request: wl_data_device::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        match request {
            wl_data_device::Request::SetSelection { source, .. } => {
                if let Some(old) = state.selection.take() {
                    if Some(&old) != source.as_ref() {
                        old.cancelled();
                    }
                }
                state.selection = source;
                state.send_selection();
            }
            // Drag and drop isn't supported yet: tell the app it didn't happen.
            wl_data_device::Request::StartDrag { source: Some(s), .. } => s.cancelled(),
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, OfferData> for State {
    fn request(_: &mut Self, _: &Client, _: &WlDataOffer, request: wl_data_offer::Request, data: &OfferData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        // Paste: the copying app writes into the pasting app's pipe.
        if let wl_data_offer::Request::Receive { mime_type, fd } = request {
            data.source.send(mime_type, fd.as_fd());
        }
    }
}
