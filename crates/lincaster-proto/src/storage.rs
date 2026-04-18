//! Device storage discovery for the RØDECaster.
//!
//! The RØDECaster exposes its pad sound storage as a USB Mass Storage
//! device (EP4) that Linux auto-mounts as a FAT filesystem. This module
//! detects the mount point and resolves pad sound file paths.

use std::path::{Path, PathBuf};

/// Find the mount point of the RØDECaster's internal storage.
///
/// Strategy:
/// 1. Scan `/sys/block/` for a block device with model "File-Stor Gadget"
///    and vendor "Linux" (the RØDECaster's USB Mass Storage identity).
/// 2. Look up its mount point from `/proc/mounts`.
/// 3. Verify the mount has a `pads/` directory.
///
/// Returns `None` if the device storage is not found or not mounted.
pub fn find_device_mount() -> Option<PathBuf> {
    let block_dev = find_storage_block_device()?;
    let mount_point = find_mount_point(&block_dev)?;

    // Verify it's actually the RØDECaster storage
    if mount_point.join("pads").is_dir() {
        Some(mount_point)
    } else {
        None
    }
}

/// Unmount the RØDECaster's device storage via udisksctl.
///
/// Finds the block device and unmounts it, mirroring what the Windows
/// RØDE Central app does when exiting transfer mode.
pub fn unmount_device_storage() -> std::io::Result<()> {
    let block_dev = find_storage_block_device().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "RØDECaster storage block device not found",
        )
    })?;

    let dev_path = format!("/dev/{}", block_dev);

    // Check if actually mounted
    let mounts = std::fs::read_to_string("/proc/mounts")?;
    let is_mounted = mounts.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() >= 2 && parts[0].starts_with(&dev_path)
    });

    if !is_mounted {
        return Ok(());
    }

    let out = std::process::Command::new("udisksctl")
        .args(["unmount", "-f", "-b", &dev_path])
        .output()?;

    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "udisksctl unmount failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        ))
    }
}

/// Mount the RØDECaster's device storage via udisksctl.
///
/// Waits for the block device to appear (up to 10s), then mounts it
/// read-write. Returns the mount point path on success.
pub fn mount_device_storage() -> std::io::Result<PathBuf> {
    // Wait for the block device to appear
    let block_dev = {
        let mut found = None;
        for _ in 0..20 {
            if let Some(dev) = find_storage_block_device() {
                found = Some(dev);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        found.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "RØDECaster storage block device not found after 10s",
            )
        })?
    };

    let dev_path = format!("/dev/{}", block_dev);

    // Check if already mounted
    if let Some(mount) = find_mount_point(&block_dev) {
        if mount.join("pads").is_dir() {
            return Ok(mount);
        }
    }

    // Mount via udisksctl (unprivileged).  The device may need a moment
    // after entering transfer mode before udisks2 recognises the filesystem,
    // so retry a few times with a short delay.
    let mut last_err = String::new();
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let out = std::process::Command::new("udisksctl")
            .args(["mount", "-b", &dev_path])
            .output()?;

        if out.status.success() {
            // Poll for the mount point to appear in /proc/mounts
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Some(mount) = find_mount_point(&block_dev) {
                    if mount.join("pads").is_dir() {
                        return Ok(mount);
                    }
                }
            }

            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Mount point did not appear after udisksctl mount",
            ));
        }

        last_err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("udisksctl mount failed: {}", last_err),
    ))
}

/// Find the block device name (e.g., "sdb") for the RØDECaster storage.
fn find_storage_block_device() -> Option<String> {
    let block_dir = Path::new("/sys/block");
    let entries = std::fs::read_dir(block_dir).ok()?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();

        // Only check sd* devices (SCSI/USB mass storage)
        if !name.starts_with("sd") {
            continue;
        }

        let device_dir = entry.path().join("device");

        // Check vendor
        let vendor_path = device_dir.join("vendor");
        let vendor = std::fs::read_to_string(&vendor_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        if vendor != "Linux" {
            continue;
        }

        // Check model
        let model_path = device_dir.join("model");
        let model = std::fs::read_to_string(&model_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        if model.contains("File-Stor Gadget") {
            return Some(name);
        }
    }

    None
}

/// Ensure the device storage mount is writable.
///
/// udisks2 often auto-mounts FAT USB storage as read-only. This checks
/// `/proc/mounts` for `ro` and remounts read-write if needed. We first
/// try the unprivileged `udisksctl` approach; if that's unavailable we
/// fall back to `mount -o remount,rw` (requires appropriate permissions
/// or a polkit rule).
pub fn ensure_mount_writable(mount: &Path) -> std::io::Result<()> {
    let mount_str = mount.to_string_lossy();

    // Check if already mounted rw
    let mounts = std::fs::read_to_string("/proc/mounts")?;
    let is_ro = mounts.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() >= 4
            && parts[1] == mount_str.as_ref()
            && parts[3].split(',').any(|opt| opt == "ro")
    });

    if !is_ro {
        return Ok(());
    }

    // Find the block device for this mount
    let dev = mounts
        .lines()
        .find_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == mount_str.as_ref() {
                Some(parts[0].to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "device storage mount not found in /proc/mounts",
            )
        })?;

    // Unmount and re-mount read-write via udisksctl (unprivileged, no sudo).
    // We must wait for the device to truly disappear from /proc/mounts
    // before re-mounting, otherwise udisks2's automounter races us and
    // the mount call fails with "already mounted".
    let unmount = std::process::Command::new("udisksctl")
        .args(["unmount", "-f", "-b", &dev])
        .output();

    match unmount {
        Ok(out) if out.status.success() => {
            // Poll until the device is truly unmounted (up to 3 s)
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
                let still_mounted = mounts.lines().any(|line| {
                    line.split_whitespace().next() == Some(dev.as_str())
                });
                if !still_mounted {
                    break;
                }
            }

            let mount_out = std::process::Command::new("udisksctl")
                .args(["mount", "-b", &dev, "-o", "rw"])
                .output()?;
            if mount_out.status.success() {
                return Ok(());
            }
            // If rw mount failed, try without -o (some udisksctl versions)
            let mount_out2 = std::process::Command::new("udisksctl")
                .args(["mount", "-b", &dev])
                .output()?;
            if mount_out2.status.success() {
                return Ok(());
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "udisksctl mount failed: {}",
                    String::from_utf8_lossy(&mount_out2.stderr)
                ),
            ))
        }
        _ => {
            // udisksctl unavailable or unmount failed — try mount remount
            let remount = std::process::Command::new("mount")
                .args(["-o", "remount,rw", &mount_str.to_string()])
                .output()?;
            if remount.status.success() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "Cannot remount device storage read-write: {}",
                        String::from_utf8_lossy(&remount.stderr)
                    ),
                ))
            }
        }
    }
}

/// Find the mount point for a given block device name (e.g., "sdb").
fn find_mount_point(block_dev: &str) -> Option<PathBuf> {
    let dev_path = format!("/dev/{}", block_dev);
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[0] == dev_path {
            return Some(PathBuf::from(parts[1]));
        }
    }

    // Also check for partition-style mounts (e.g., /dev/sdb1)
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[0].starts_with(&dev_path) {
            return Some(PathBuf::from(parts[1]));
        }
    }

    None
}

/// Get the directory path for a specific pad on device storage.
///
/// `pad_idx` is the logical pad index: `bank * pads_per_bank + position`.
/// The device uses 1-based pad directory numbering.
pub fn pad_dir(mount: &Path, pad_idx: usize) -> PathBuf {
    mount.join("pads").join(format!("{}", pad_idx + 1))
}

/// Find the sound file for a specific pad on device storage.
///
/// Returns the path to the first .wav or .mp3 file found in the pad's
/// directory, or `None` if no sound file exists.
pub fn find_pad_sound_file(mount: &Path, pad_idx: usize) -> Option<PathBuf> {
    let dir = pad_dir(mount, pad_idx);
    let entries = std::fs::read_dir(&dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_lower = ext.to_ascii_lowercase();
            if ext_lower == "wav" || ext_lower == "mp3" {
                return Some(path);
            }
        }
    }

    None
}

/// Import a sound file to a pad's storage directory on the device.
///
/// Copies the source file to `<mount>/pads/<padIdx+1>/`. WAV files keep
/// their original filename; MP3 files are renamed to `sound.mp3` (the
/// device only recognises that name for MP3).
/// Returns the device-side relative path (e.g., `pads/3/sound.mp3`).
pub fn import_sound_file(
    mount: &Path,
    pad_idx: usize,
    source: &Path,
) -> std::io::Result<String> {
    // Note: caller is responsible for ensuring the mount is writable
    // (calling ensure_mount_writable before this).  Doing it here would
    // trigger an unmount/remount cycle that could invalidate the mount path.

    let dir = pad_dir(mount, pad_idx);

    // Create the pad directory if it doesn't exist
    std::fs::create_dir_all(&dir)?;

    // Remove any existing sound files in the pad directory
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_ascii_lowercase();
                if ext_lower == "wav" || ext_lower == "mp3" {
                    std::fs::remove_file(&path)?;
                }
            }
        }
    }

    // WAV files keep their original name; MP3 files must be named sound.mp3
    let src_ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let dest_name = if src_ext == "wav" {
        source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    } else {
        "sound.mp3".to_string()
    };
    let dest = dir.join(&dest_name);
    let src_size = std::fs::metadata(source)?.len();
    let copied = std::fs::copy(source, &dest)?;
    if copied != src_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Incomplete copy: wrote {} of {} bytes", copied, src_size),
        ));
    }

    // Flush all data to the USB mass storage device.  Without this the
    // kernel may still have dirty pages buffered when the daemon sends
    // remountPadStorage, causing the device to see a truncated/corrupt file.
    {
        let f = std::fs::File::open(&dest)?;
        f.sync_all()?;
    }
    // Also sync the directory entry (FAT32 needs this for new files).
    if let Ok(d) = std::fs::File::open(&dir) {
        let _ = d.sync_all();
    }

    // Verify the file on device matches source size
    let dest_size = std::fs::metadata(&dest)?.len();
    if dest_size != src_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Size mismatch after sync: dest {} vs source {} bytes", dest_size, src_size),
        ));
    }

    // Return device-relative path
    let pad_num = pad_idx + 1;
    Ok(format!("pads/{}/{}", pad_num, dest_name))
}

/// Export a pad's sound file to a local destination path.
///
/// Copies from device storage to the user-chosen location.
pub fn export_sound_file(
    mount: &Path,
    pad_idx: usize,
    dest: &Path,
) -> std::io::Result<()> {
    let source = find_pad_sound_file(mount, pad_idx)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No sound file found for pad {}", pad_idx + 1),
            )
        })?;
    std::fs::copy(&source, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_dir() {
        let mount = Path::new("/run/media/user/6BA0-39A1");
        assert_eq!(
            pad_dir(mount, 0),
            PathBuf::from("/run/media/user/6BA0-39A1/pads/1")
        );
        assert_eq!(
            pad_dir(mount, 5),
            PathBuf::from("/run/media/user/6BA0-39A1/pads/6")
        );
        assert_eq!(
            pad_dir(mount, 47),
            PathBuf::from("/run/media/user/6BA0-39A1/pads/48")
        );
        assert_eq!(
            pad_dir(mount, 63),
            PathBuf::from("/run/media/user/6BA0-39A1/pads/64")
        );
    }

    #[test]
    fn test_find_device_mount_live() {
        // This test only passes when the device is actually connected
        if let Some(mount) = find_device_mount() {
            eprintln!("Found device mount at: {}", mount.display());
            assert!(mount.join("pads").is_dir());

            // Try to find a sound file for pad 0
            if let Some(sound) = find_pad_sound_file(&mount, 0) {
                eprintln!("Pad 1 sound file: {}", sound.display());
            }
        } else {
            eprintln!("No device storage found (device not connected?)");
        }
    }
}
