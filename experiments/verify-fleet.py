"""Independent, read-only verification: ext4 allocation and exact byte sets.

uv run --no-project python experiments/verify-fleet.py RUN t0|t1|t2
"""
import gc
import json
import mmap
import re
import sys
from pathlib import Path


def allocation(directory):
    table = json.loads((directory / "root.partitions.json").read_text())["partitiontable"]
    partition, = [p for p in table["partitions"] if p["type"] == "0FC63DAF-8483-4772-8E79-3D69D8477DE4"]
    offset = partition["start"] * table["sectorsize"]
    text = (directory / "root.allocation.txt").read_text()
    field = lambda key: int(re.search(r"^" + key + r":\s+(\d+)", text, re.M)[1])
    count, size = field("Block count"), field("Block size")
    free = bytearray(count)
    for line in re.findall(r"^  Free blocks:([^\n]*)", text, re.M):
        for entry in line.split(","):
            if not entry.strip():
                continue
            bounds = list(map(int, entry.strip().split("-")))
            start, end = bounds[0], bounds[-1]
            assert 0 <= start <= end < count and not any(free[start:end+1])
            free[start:end+1] = b"\1" * (end-start+1)
    assert sum(free) == field("Free blocks")
    punched = bytearray(count)
    for line in (directory / "root.free-ranges.txt").read_text().splitlines():
        start, length = map(int, line.split())
        assert start % size == length % size == 0
        punched[start//size:(start+length)//size] = b"\1" * (length//size)
    assert punched == free
    nonzero_free = 0
    with (directory / "image.raw").open("rb") as original, (directory / "root.root.raw").open("rb") as normalized:
        with mmap.mmap(original.fileno(), 0, access=mmap.ACCESS_READ) as before, mmap.mmap(normalized.fileno(), 0, access=mmap.ACCESS_READ) as after:
            assert len(after) == count * size
            zero = bytes(size)
            for block, unused in enumerate(free):
                start = block * size
                old = before[offset+start:offset+start+size]
                assert after[start:start+size] == (zero if unused else old), (directory, block)
                nonzero_free += bool(unused and old != zero)
    assert (directory / "guest/exit-code").read_text().strip() == "0"
    return dict(filesystem_blocks=count, free_blocks=sum(free), nonzero_free_blocks_excluded=nonzero_free,
                every_allocated_block_unchanged=True, every_free_block_zero=True)


def chunks(path, size):
    unique = set()
    total = 0
    with path.open("rb") as stream:
        while chunk := stream.read(size):
            if chunk != bytes(len(chunk)):
                unique.add(chunk)
                total += len(chunk)
    return unique, total


def packages(path):
    return dict(line.split("\t") for line in path.read_text().splitlines())


def verify(run, epoch):
    report = json.loads((run / f"{epoch}.json").read_text())
    paths = list(dict.fromkeys(Path(image["path"]) for image in report["results"][0]["images"]))
    assert all(path.is_relative_to(run) for path in paths)
    maps = {str(path.parent.relative_to(run)): allocation(path.parent) for path in paths}
    counts = []
    weight = lambda values: sum(map(len, values))
    for result in report["results"]:
        size = result["chunk_bytes"]
        content = {path: chunks(path, size) for path in paths}
        images = result["images"]
        for image in images:
            values, total = content[Path(image["path"])]
            assert total == image["nonzero_chunk_bytes"]
            assert weight(values) == image["unique_bytes"]
        base = content[Path(images[0]["path"])][0]
        children = [content[Path(image["path"])][0] for image in images[1:]]
        combined = set.union(*children)
        exact = dict(
            independently_unique_bytes=sum(weight(child) for child in children),
            fleet_unique_bytes=weight(combined),
            base_present_unique_bytes=weight(combined & base),
            base_content_duplicate_bytes=sum(weight(child & base) for child in children)-weight(combined & base),
            novel_content_duplicate_bytes=sum(weight(child-base) for child in children)-weight(combined-base),
        )
        assert exact == result["descendants"], (epoch, size)
        assert exact["independently_unique_bytes"] == exact["fleet_unique_bytes"] + exact["base_content_duplicate_bytes"] + exact["novel_content_duplicate_bytes"]
        counts.append(dict(chunk_bytes=size, exact_byte_sets_match=True, **exact))
        del content, base, children, combined, values
        gc.collect()
    changes = {}
    current = []
    for role in ("web", "cache", "database") if epoch != "t0" else ("base",):
        directory = run / epoch / role if epoch != "t0" else run / "base"
        before = packages(directory / "guest/packages-before.tsv")
        after = packages(directory / "guest/packages-after.tsv")
        if epoch == "t0":
            assert before == after, "base preparation installed packages"
        else:
            parent = run / "base" if epoch == "t1" else run / "t1" / role
            assert before == packages(parent / "guest/packages-after.tsv"), "package lineage changed before workload"
            assert before != after, "update installed no changes"
            assert {"web": "nginx-light", "cache": "redis-server", "database": "postgresql-client"}[role] in after
        changes[role] = dict(added=len(after.keys()-before.keys()), removed=len(before.keys()-after.keys()),
                             changed=sum(before[key] != after[key] for key in before.keys() & after.keys()))
        current.append(after)
    common = set.intersection(*(set(state) for state in current))
    assert all(len({state[package] for state in current}) == 1 for package in common)
    return dict(epoch=epoch, allocation=maps, census=counts, packages=changes,
                common_package_versions_agree=True, common_packages=len(common), passed=True)


if __name__ == "__main__":
    print(json.dumps(verify(Path(sys.argv[1]).resolve(), sys.argv[2]), indent=2))
