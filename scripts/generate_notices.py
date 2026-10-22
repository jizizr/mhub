#!/usr/bin/env python3
"""Regenerate third-party notices after npm ci and cargo fetch (Python 3.11+)."""
from pathlib import Path
import json
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def license_texts(directory):
    paths = sorted(p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(('license', 'licence', 'copying', 'notice')))
    return '\n\n'.join(p.name + '\n' + p.read_text(errors='replace') for p in paths)


def main():
    frontend = ['MHub — frontend third-party notices', 'Packages retain their original copyrights and licenses.', '']
    lock = json.loads((ROOT / 'frontend/package-lock.json').read_text())
    for name, info in sorted(lock['packages'].items()):
        if not name or info.get('dev'):
            continue
        directory = ROOT / 'frontend' / name
        if not directory.is_dir():
            raise SystemExit('Run npm ci in frontend before generating notices')
        frontend += ['=' * 72, f"{name.removeprefix('node_modules/')} {info.get('version', '')} ({info.get('license', 'see package')})", license_texts(directory), '']
    (ROOT / 'frontend/public/legal/third-party-notices.txt').write_text('\n'.join(frontend) + '\n')
    packages = {}
    for manifest in ['Cargo.toml', 'wasm/alac-decoder/Cargo.toml']:
        result = subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--manifest-path', manifest], cwd=ROOT)
        for package in json.loads(result)['packages']:
            if package['source']:
                packages[(package['name'], package['version'])] = package
    rust = ['MHub — Rust dependency notices', 'Exact versions are recorded in the application and decoder Cargo.lock files.', 'Dependencies retain their own copyrights and licenses.', '']
    for (name, version), info in sorted(packages.items()):
        directory = Path(info['manifest_path']).parent
        rust += ['=' * 72, f"{name} {version} ({info.get('license') or 'see crate'})", info.get('repository') or '', license_texts(directory), '']
    (ROOT / 'THIRD_PARTY_NOTICES.txt').write_text('\n'.join(rust) + '\n')
    print(f'Generated notices for {len(packages)} Rust packages and frontend runtime dependencies')


if __name__ == '__main__':
    main()
