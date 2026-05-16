use nom::HexDisplay;
use nusb::transfer::{Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::{MaybeFuture, list_devices};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::ops::Deref;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zerocopy::FromBytes;
use zerocopy_derive::{FromBytes, Immutable, KnownLayout};

#[repr(C, packed)]
#[derive(FromBytes, Default, Immutable, KnownLayout, Debug)]
struct KisHeader {
    sequence: u16,        // A sequence number
    version: u8,          // Protocol version
    portal: u8,           // The "portal" to connect to
    argCount: u8,         // Number of arguments
    indexLo: u8,          // An index
    indexHiRplSizeLo: u8, // High 2 bits of index + low 6 bytes of reply size
    rplSizeHi: u8,        // Reply size high bits: , number of words the device should send
    reqSize: u32, // Size of the complete request: , including the arguments and payload: , excluding the header
                  // Followed by arguments and payload data
}

async fn debugusb_loop() -> anyhow::Result<()> {
    let device = list_devices()
        .await?
        .find(|dev| dev.vendor_id() == 0x05ac && dev.product_id() == 0x1881)
        .ok_or(anyhow::anyhow!("device not found"))?;
    println!("open");
    let device = device.open().await?;
    println!("set_configuration");
    /*
        for config in device.configurations() {
            println!("{:#?}", config);
            println!("{}", config.configuration_value());
        }

        return Ok(());
    */
    device.set_configuration(1).await?;
    println!("claim_interface");

    let interface = device.claim_interface(0).await?;

    let mut rx_ep = interface.endpoint::<Bulk, In>(0x84)?;
    tokio::task::spawn(async move {
        let mut buf = None;
        loop {
            rx_ep.submit(buf.take().unwrap_or(rx_ep.allocate(262144)));
            let retbuf = rx_ep.next_complete().await.into_result()?;
            let (hdr, payload) = KisHeader::ref_from_prefix(&retbuf).unwrap();

            if hdr.portal == 0x13 {
                let words = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
                let bytes = u32::from_le_bytes(payload[
                    (words+1)*4..(words+2)*4
                ].try_into().unwrap()) as usize;
            //    println!("words: {}, bytes: {}", words, bytes);
                std::io::Write::write_all(&mut std::io::stdout(), &payload[4..4+bytes])?;
            } else {
            println!("in 4:\n{:#x?}\n{}", hdr, payload.to_hex(16));
            }
            buf = Some(retbuf);
        }
        Ok::<_, anyhow::Error>(())
    });

    let mut tx = HashMap::new();
    let mut rx = HashMap::new();

    let tx_ep = interface.endpoint::<Bulk, Out>(0x1)?;
    let mut writer = tx_ep.writer(262144);
    //let mut reader = rx_ep.reader(262144);
    tx.insert(1, writer);

    let mut rx_ep = interface.endpoint::<Bulk, In>(0x81)?;
    let mut buf = None;
    //let mut reader = rx_ep.reader(262144);
    rx.insert(1, (rx_ep, buf));

    let tx_ep = interface.endpoint::<Bulk, Out>(0x3)?;
    let mut writer = tx_ep.writer(262144);
    tx.insert(3, writer);

    let mut rx_ep = interface.endpoint::<Bulk, In>(0x83)?;
    let mut buf = None;
    rx.insert(3, (rx_ep, buf));

    for (ep, msg) in &[
        (1,&b"\x02\xff\xa0\x01\x01\x16\x04\x00\x04\x00\x00\x00\x00\x00\x03\x00"[..]),
        (3,&b"\x00\x13\xa0\x13\x01\x03\x04\x00\x04\x00\x00\x00\x01\x00\x00\x00"[..]),
    ] {
        println!("out {}:\n{}", ep, msg.to_hex(16));
        let hdr = KisHeader::ref_from_prefix(msg).unwrap().0;
        println!("{:#x?}", hdr);
        tx.get_mut(ep).unwrap().write_all(msg).await?;
        tx.get_mut(ep).unwrap().flush().await?;
        //tokio::time::sleep(Duration::from_millis(150)).await;

        let (rx_ep, buf) = rx.get_mut(ep).unwrap();

        rx_ep.submit(buf.take().unwrap_or(rx_ep.allocate(262144)));
        let Ok(retbuf) = tokio::time::timeout(Duration::from_millis(100), rx_ep.next_complete()).await else {
            println!("ep {} timeout", ep);
            continue;
        };
        let retbuf = retbuf.into_result()?;
        println!("in {}:\n", ep); //, (&retbuf).to_hex(16));
        let hdr = KisHeader::ref_from_prefix(&retbuf).unwrap().0;
        println!("{:#x?}", hdr);
        *buf = Some(retbuf);
    }

    loop {
        let (rx_ep, buf) = rx.get_mut(&1).unwrap();
        rx_ep.submit(buf.take().unwrap_or(rx_ep.allocate(262144)));
        let retbuf = rx_ep.next_complete().await.into_result()?;
        println!("in 1:\n{}", (&retbuf).to_hex(16));
        *buf = Some(retbuf);
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    loop {
        if let Err(e) = debugusb_loop().await {
            println!("{:?}", e);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
