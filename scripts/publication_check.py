#!/usr/bin/env python3
"""Check publication boundaries without printing matching secret values."""
from pathlib import Path
import re
import subprocess
import sys

ROOT_FILES = {
    '.gitignore', '.gitattributes', '.dockerignore', '.env.example', 'Cargo.toml', 'Cargo.lock',
    'Dockerfile', 'compose.yaml', 'config.example.toml', 'README.md',
    'CONTRIBUTING.md', 'SECURITY.md', 'LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.txt',
}
DOCS = {'docs/deployment.md', 'docs/architecture.md'}
PATTERNS = {
    'private key': re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'),
    'Telegram token': re.compile(rb'\b[0-9]{8,12}:[A-Za-z0-9_-]{30,}\b'),
    'GitHub token': re.compile(rb'\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b'),
    'Google API key': re.compile(rb'AIza[0-9A-Za-z_-]{35}'),
    'AWS access key': re.compile(rb'\bAKIA[0-9A-Z]{16}\b'),
    'embedded device credential': re.compile(rb'V1ZE[A-Za-z0-9+/]{128,}={0,2}'),
    'embedded session JWT': re.compile(rb'eyJ[A-Za-z0-9_-]{40,}\.[A-Za-z0-9_-]{40,}\.[A-Za-z0-9_-]{40,}'),
    'machine path': re.compile(rb'/(?:Users|home)/[A-Za-z0-9_.-]+/'),
    'credentialed URL': re.compile(rb'https?://[^\s/"\'<>]+:[^\s/"\'<>]+@'),
}
EXAMPLE_URL = re.compile(rb'https?://(?:name:secret|user:password|user:secret)@[^\s/"\'<>]*(?:example(?:\.test)?|localhost)|https' rb'://user:password@(?=[\'\"])')


def allowed(path):
    p = Path(path)
    if p.is_absolute() or '..' in p.parts or p.is_symlink():
        return False
    if path in ROOT_FILES or path in DOCS:
        return True
    if any(part in {'node_modules', 'target', '__pycache__', 'docs', 'pkg'} for part in p.parts):
        return False
    if p.name.startswith('.env') or p.name == '.DS_Store' or p.suffix in {'.pem', '.key', '.log', '.sqlite', '.db', '.har', '.wvd', '.bin'}:
        return False
    if path.startswith(('src/', 'frontend/src/', 'frontend/tests/', 'frontend/public/', '.github/workflows/')):
        return True
    if path in {'frontend/package.json', 'frontend/package-lock.json', 'frontend/index.html', 'frontend/vite.config.js', 'examples/alac_packet_stream.rs'}:
        return True
    if path.startswith('scripts/') and p.suffix in {'.mjs', '.py'}:
        return True
    if path.startswith('wasm/alac-decoder/src/'):
        return True
    return path in {f'wasm/alac-decoder/{name}' for name in ['Cargo.toml', 'Cargo.lock', 'README.md', 'build.sh']}


def public_files(root):
    # Walk only known source roots; never enumerate runtime/account storage.
    for name in ROOT_FILES | DOCS:
        if (root / name).is_file():
            yield name
    for directory in ['src', 'frontend/src', 'frontend/tests', 'frontend/public', 'scripts', '.github/workflows', 'wasm/alac-decoder/src']:
        for item in (root / directory).rglob('*'):
            relative = item.relative_to(root).as_posix()
            if item.is_file() and allowed(relative):
                yield relative
    for name in ['frontend/package.json', 'frontend/package-lock.json', 'frontend/index.html', 'frontend/vite.config.js', 'examples/alac_packet_stream.rs'] + [f'wasm/alac-decoder/{n}' for n in ['Cargo.toml', 'Cargo.lock', 'README.md', 'build.sh']]:
        if (root / name).is_file():
            yield name


def check_files(root, files, staged=False):
    problems = []
    for name in files:
        if not allowed(name) or (root / name).is_symlink():
            problems.append((name, 'outside publication allowlist'))
            continue
        content = subprocess.check_output(['git', 'show', f':{name}'], cwd=root) if staged else (root / name).read_bytes()
        # Negative URL-validation tests contain explicitly fictional credentials.
        if name.startswith(('frontend/tests/', 'src/')):
            content = EXAMPLE_URL.sub(b'https://example.test', content)
        for kind, pattern in PATTERNS.items():
            if pattern.search(content):
                problems.append((name, kind))
    if problems:
        for name, kind in problems:
            print(f'{name}: {kind}', file=sys.stderr)
        raise SystemExit('Publication check failed; matching values are intentionally hidden.')


def main():
    root = Path(__file__).resolve().parent.parent
    staged = '--staged' in sys.argv
    if staged:
        files = subprocess.check_output(['git', 'ls-files', '-z'], cwd=root).decode().split('\0')
        files = [f for f in files if f]
    else:
        files = list(public_files(root))
    check_files(root, files, staged=staged)
    print(f'Publication check passed: {len(files)} files')


if __name__ == '__main__':
    main()
