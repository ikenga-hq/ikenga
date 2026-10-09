---
"ikenga-desktop": minor
---

`provision.sh swap`: a swap file for small server boxes, also run first in the full provision run. Profile `SWAP_SIZE` (`auto` = 4G up to 8 GB RAM else 2G, or `2G`/`4096M`, or `off`/`0` to remove what it made), `SWAPPINESS` (default 10) and `SWAP_FILE` (default `/swapfile`). If any other swap is already active it changes nothing and says so. Otherwise it builds the file next to its final name (`fallocate`, `dd` fallback), `mkswap`, renames it into place, adds a single `# ikenga-swap` line to `/etc/fstab` (backed up first, never duplicated) and writes `vm.swappiness` to `/etc/sysctl.d/90-ikenga-swap.conf`. A resize or removal only turns swap off when what is in use fits back in free memory. Refuses a symlinked path, a non-ext4/xfs/f2fs filesystem (btrfs, tmpfs, NFS...), a file that already exists unmanaged, and anything that would leave under 10% of the disk free.
