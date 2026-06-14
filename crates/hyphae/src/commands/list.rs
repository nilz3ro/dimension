use std::fmt;

use anyhow::{Context, Result};
use serde::Serialize;

use hyphae_core::registry::storage::default_data_dir;
use hyphae_core::registry::Registry;

use crate::cli::{ListArgs, ListResource};
use crate::output::OutputConfig;

// ---------------------------------------------------------------------------
// Bundle list types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BundleListOutput {
    bundles: Vec<BundleEntry>,
}

#[derive(Serialize)]
struct BundleEntry {
    id: i64,
    name: String,
    tag: String,
    content_hash: String,
    size_bytes: u64,
    source_path: String,
    default_vcpus: i64,
    default_memory_mib: i64,
    created_at: i64,
}

impl fmt::Display for BundleListOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bundles.is_empty() {
            return write!(f, "No bundles registered.");
        }

        for (i, b) in self.bundles.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            let hash_short = if b.content_hash.len() > 12 {
                &b.content_hash[..12]
            } else {
                &b.content_hash
            };
            writeln!(f, "{}:{}", b.name, b.tag)?;
            writeln!(f, "  id:      {}", b.id)?;
            writeln!(f, "  hash:    {hash_short}")?;
            writeln!(f, "  size:    {} bytes", b.size_bytes)?;
            writeln!(f, "  source:  {}", b.source_path)?;
            writeln!(f, "  vcpus:   {}", b.default_vcpus)?;
            writeln!(f, "  memory:  {} MiB", b.default_memory_mib)?;
            write!(f, "  created: {}", format_timestamp(b.created_at))?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// VM list types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct VmListOutput {
    vms: Vec<VmEntry>,
}

#[derive(Serialize)]
struct VmEntry {
    vm_id: String,
    bundle: String,
    image_id: i64,
    started_at: i64,
}

impl fmt::Display for VmListOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.vms.is_empty() {
            return write!(f, "No running VMs.");
        }

        for (i, vm) in self.vms.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            writeln!(f, "{}", vm.vm_id)?;
            writeln!(f, "  bundle:  {}", vm.bundle)?;
            write!(f, "  started: {}", format_timestamp(vm.started_at))?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Timestamp formatting
// ---------------------------------------------------------------------------

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// Uses manual arithmetic to avoid adding a chrono dependency.
fn format_timestamp(epoch_secs: i64) -> String {
    if epoch_secs < 0 {
        return epoch_secs.to_string();
    }

    let secs = epoch_secs as u64;
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    // Compute year/month/day from days since epoch (1970-01-01).
    let mut year: u64 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month: u64 = 1;
    for md in &month_days {
        if days < *md {
            break;
        }
        days -= *md;
        month += 1;
    }
    let day = days + 1;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} UTC")
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

pub async fn execute(args: ListArgs, output: &OutputConfig) -> Result<()> {
    match args.resource {
        ListResource::Bundles => list_bundles(output).await,
        ListResource::Vms => list_vms(output).await,
    }
}

async fn list_bundles(output: &OutputConfig) -> Result<()> {
    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;

    let images = registry
        .list_images(None, None, None, None)
        .context("Failed to list bundles")?;

    let bundles: Vec<BundleEntry> = images
        .into_iter()
        .map(|img| BundleEntry {
            id: img.id,
            name: img.name,
            tag: img.tag,
            content_hash: img.content_hash,
            size_bytes: img.size_bytes,
            source_path: img.source_path,
            default_vcpus: img.default_vcpus,
            default_memory_mib: img.default_memory_mib,
            created_at: img.created_at,
        })
        .collect();

    let list_output = BundleListOutput { bundles };
    output.print_result(&list_output);

    Ok(())
}

async fn list_vms(output: &OutputConfig) -> Result<()> {
    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;

    let vm_records = registry
        .list_all_vms()
        .context("Failed to list running VMs")?;

    let vms: Vec<VmEntry> = vm_records
        .into_iter()
        .map(|vm| {
            let bundle = match registry.find_by_id(vm.image_id) {
                Ok(Some(img)) => format!("{}:{}", img.name, img.tag),
                _ => "<deleted>".to_string(),
            };

            VmEntry {
                vm_id: vm.vm_id,
                bundle,
                image_id: vm.image_id,
                started_at: vm.started_at,
            }
        })
        .collect();

    let list_output = VmListOutput { vms };
    output.print_result(&list_output);

    Ok(())
}
