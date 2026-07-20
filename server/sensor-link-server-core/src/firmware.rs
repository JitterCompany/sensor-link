use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    store_traits::{FirmwareStore, Result},
    DataStoreId,
};

#[derive(Debug, Deserialize, Serialize)]
#[allow(non_snake_case)]
pub struct NewFirmware<D> {
    pub version: String,
    pub description: String,
    pub date: DateTime<Utc>,
    pub v2BinID: String,
    pub recommended: bool,
    pub device_type: D,
}

#[derive(Debug, Deserialize, Serialize)]
#[allow(non_snake_case)]
pub struct Firmware<D> {
    #[serde(rename = "_id")]
    pub id: String,
    pub version: String,
    pub description: String,
    pub date: DateTime<Utc>,
    pub v2BinID: String,
    pub recommended: bool,
    pub device_type: D,
}

pub async fn recommend_firmware<D: Serialize + 'static>(
    datastore: &dyn FirmwareStore<DeviceType = D>,
    id: &DataStoreId,
) -> Result<()> {
    datastore
        .unrecommend_other_firmwares_for_same_device_type(id)
        .await?;
    datastore.recommend_firmware(id).await?;
    Ok(())
}
