//! Custom async AT client implementation.
//!

use atat::{
    asynch::AtatClient, response_slot::ResponseSlotGuard, AtatCmd, AtatResp, Error, InternalError,
    Response, ResponseSlot,
};
use embedded_io_async::Write;

use crate::{
    monotonic_time::{self, FutureTimeout},
    utils::LossyStr,
};

/// Client for communicating with a modem via AT commands.
pub struct ATClient<'a, W: Write, const INGRESS_BUF_SIZE: usize> {
    writer: W,
    timer: monotonic_time::Timeout,
    buf: &'a mut [u8],
    res_slot: &'a ResponseSlot<INGRESS_BUF_SIZE>,
}

impl<'a, W: Write, const INGRESS_BUF_SIZE: usize> ATClient<'a, W, INGRESS_BUF_SIZE> {
    pub fn new(writer: W, res_slot: &'a ResponseSlot<INGRESS_BUF_SIZE>, buf: &'a mut [u8]) -> Self {
        Self {
            writer,
            res_slot,
            timer: monotonic_time::timeout(),
            buf,
        }
    }

    async fn send_request(&mut self, len: usize) -> Result<(), Error> {
        // only in debug mode
        #[cfg(debug_assertions)]
        {
            if len < 50 {
                log::debug!("Sending command: {:?}", LossyStr(&self.buf[..len]));
            } else {
                log::debug!("Sending command with long payload ({} bytes)", len);
            }
        }
        self.wait_cooldown_timer().await;

        // Clear any pending response signal
        self.res_slot.reset();

        // Write request
        self.writer
            .write_all(&self.buf[..len])
            .await
            .map_err(|_| Error::Write)?;
        self.writer.flush().await.map_err(|_| Error::Write)?;

        self.start_cooldown_timer();
        Ok(())
    }

    /// Send a slice, without any copy overhead (bypassing self.buf)
    async fn send_slice(&mut self, slice: &[u8]) -> Result<(), Error> {
        self.wait_cooldown_timer().await;

        // Clear any pending response signal
        self.res_slot.reset();

        // Write request
        self.writer
            .write_all(slice)
            .await
            .map_err(|_| Error::Write)?;
        self.writer.flush().await.map_err(|_| Error::Write)?;

        self.start_cooldown_timer();
        Ok(())
    }

    async fn wait_response<'guard>(
        &'guard mut self,
        timeout: u32,
    ) -> Result<ResponseSlotGuard<'guard, INGRESS_BUF_SIZE>, Error> {
        let res = self.res_slot.get().with_timeout_ms(timeout).await;
        match res {
            Some(res) => Ok(res),
            None => Err(Error::Timeout),
        }
    }

    fn start_cooldown_timer(&mut self) {
        self.timer.set_ms(1);
    }

    async fn wait_cooldown_timer(&mut self) {
        self.timer.wait().await;
    }

    /// Sends bytes to the modem and await a response
    ///
    /// The byte slice is used without copying and there is no upper limit
    /// on its length.
    pub async fn send_bytes<R: AtatResp + serde::de::DeserializeOwned>(
        &mut self,
        bytes: &[u8],
        timeout_ms: u32,
    ) -> Result<R, Error> {
        // Directly send the slice, without copy to internal buffer
        self.send_slice(bytes).await?;
        self.expect_response(timeout_ms).await
    }

    /// Wait for a response.
    async fn expect_response<R: AtatResp + serde::de::DeserializeOwned>(
        &mut self,
        timeout_ms: u32,
    ) -> Result<R, Error> {
        let response = self.wait_response(timeout_ms).await?;

        let response: &Response<INGRESS_BUF_SIZE> = &response.borrow();
        let res: Result<&[u8], InternalError> = response.into();
        let slice = res?;
        atat::serde_at::from_slice(slice).map_err(|_| Error::Parse)
    }
}
impl<W: Write, const INGRESS_BUF_SIZE: usize> AtatClient for ATClient<'_, W, INGRESS_BUF_SIZE> {
    async fn send<Cmd: AtatCmd>(&mut self, cmd: &Cmd) -> Result<Cmd::Response, Error> {
        if Cmd::MAX_LEN > self.buf.len() {
            log::error!("Command cannot be sent: {} > buffer size!", Cmd::MAX_LEN);
            return Err(Error::Custom);
        }
        let len = cmd.write(&mut self.buf);
        match self.send_request(len).with_timeout_ms(1000).await {
            Some(res) => res?,
            None => {
                log::error!("Timeout: Failed to send command");
                return Err(Error::Timeout);
            }
        }
        if !Cmd::EXPECTS_RESPONSE_CODE {
            cmd.parse(Ok(&[]))
        } else {
            let response = self.wait_response(Cmd::MAX_TIMEOUT_MS).await?;
            let response: &Response<INGRESS_BUF_SIZE> = &response.borrow();
            cmd.parse(response.into())
        }
    }
}
