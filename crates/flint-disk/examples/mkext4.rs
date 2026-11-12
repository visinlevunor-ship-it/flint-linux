fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mib: u64 = a[2].parse().unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&a[1]).unwrap();
    f.set_len(mib << 20).unwrap();
    flint_disk::ext4::mkfs(&mut f, 0, mib << 20, &flint_disk::ext4::Options { label: "FLINTDATA".into(), uuid: None, time: 1_800_000_000 }).unwrap();
}
