#!/usr/bin/env python3
"""Package corresponding source using the same publication allowlist as the audit."""
import argparse
from pathlib import Path
import tarfile
import os
from publication_check import public_files, check_files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    files = list(public_files(root))
    check_files(root, files)
    def metadata(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ''
        info.mtime = int(os.environ.get('SOURCE_DATE_EPOCH', '0'))
        return info
    with tarfile.open(args.output, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(files):
            archive.add(root / path, arcname=f"mhub/{path}", recursive=False, filter=metadata)
    print(f"Packaged {len(files)} source files")


if __name__ == "__main__":
    main()
