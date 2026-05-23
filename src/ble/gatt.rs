// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use bluer::gatt::local::{
    Application, Characteristic, CharacteristicControl, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicWrite, CharacteristicWriteMethod, Service,
    characteristic_control, service_control,
};
use uuid::Uuid;

use crate::config::TncConfig;

const KISS_SERVICE_UUID: Uuid = Uuid::from_u128(0x00000001_ba2a_46c9_ae49_01b0961f68bb);
const KISS_TX_CHAR_UUID: Uuid = Uuid::from_u128(0x00000002_ba2a_46c9_ae49_01b0961f68bb);
const KISS_RX_CHAR_UUID: Uuid = Uuid::from_u128(0x00000003_ba2a_46c9_ae49_01b0961f68bb);

pub struct TncGattHandles {
    pub tx_control: CharacteristicControl,
    pub rx_control: CharacteristicControl,
    pub tnc_config: TncConfig,
}

pub fn build_application(tnc_configs: &[TncConfig]) -> (Application, Vec<TncGattHandles>) {
    let mut services = Vec::new();
    let mut handles = Vec::new();

    for tnc in tnc_configs {
        let (_svc_control, svc_handle) = service_control();
        let (tx_control, tx_handle) = characteristic_control();
        let (rx_control, rx_handle) = characteristic_control();

        let service = Service {
            uuid: KISS_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: KISS_TX_CHAR_UUID,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: tx_handle,
                    ..Default::default()
                },
                Characteristic {
                    uuid: KISS_RX_CHAR_UUID,
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: rx_handle,
                    ..Default::default()
                },
            ],
            control_handle: svc_handle,
            ..Default::default()
        };

        services.push(service);
        handles.push(TncGattHandles {
            tx_control,
            rx_control,
            tnc_config: tnc.clone(),
        });
    }

    let app = Application {
        services,
        ..Default::default()
    };
    (app, handles)
}
