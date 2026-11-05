use std::{env, fs, io, path::PathBuf};

const ICON_PATH: &str = "assets/umiocr.ico";
const ICON: &[u8] = include_bytes!("assets/umiocr.ico");
const ICON_GROUP_ID: u16 = 1;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed={ICON_PATH}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let resource =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR")).join("app-icon.res");
    fs::write(&resource, icon_resources())?;
    println!(
        "cargo:rustc-link-arg-bin=wf-zi-ui-bp={}",
        resource.display()
    );
    Ok(())
}

fn icon_resources() -> Vec<u8> {
    assert_eq!(&ICON[..4], &[0, 0, 1, 0], "expected an ICO file");
    let count = u16::from_le_bytes([ICON[4], ICON[5]]);
    assert_ne!(count, 0, "ICO file has no frames");
    let mut group = ICON[..6].to_vec();
    let mut resource = Vec::new();

    // Compiled Win32 resource files begin with a null resource header.
    append_resource(&mut resource, 0, 0, &[]);
    for id in 1..=count {
        let start = 6 + 16 * usize::from(id - 1);
        let entry = &ICON[start..start + 16];
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        append_resource(&mut resource, RT_ICON, id, &ICON[offset..offset + length]);

        // The .res group refers to a resource ID in place of the ICO byte offset.
        group.extend_from_slice(&entry[..12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    append_resource(&mut resource, RT_GROUP_ICON, ICON_GROUP_ID, &group);
    resource
}

fn append_resource(output: &mut Vec<u8>, kind: u16, name: u16, data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_le_bytes());
    output.extend_from_slice(&32_u32.to_le_bytes()); // fixed header with ordinal IDs
    for ordinal in [kind, name] {
        output.extend_from_slice(&u16::MAX.to_le_bytes());
        output.extend_from_slice(&ordinal.to_le_bytes());
    }
    output.extend_from_slice(&[0; 16]); // version, flags, neutral language, characteristics
    output.extend_from_slice(data);
    output.resize(output.len().next_multiple_of(4), 0);
}
