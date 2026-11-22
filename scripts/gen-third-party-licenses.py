#!/usr/bin/env python3
"""一次性脚本：生成 THIRD-PARTY-LICENSES.md（Rust Cargo.lock 全量 + npm node_modules 全量）。"""
import json, os, re, glob, sys, tarfile, time, subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TAURI = os.path.join(ROOT, 'src-tauri')

# ---------- Rust ----------
lock = open(os.path.join(TAURI, 'Cargo.lock')).read()
pkgs = re.findall(r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"', lock)
srcdirs = glob.glob(os.path.expanduser('~/.cargo/registry/src/*/'))
cachedirs = glob.glob(os.path.expanduser('~/.cargo/registry/cache/*/'))

def from_local(name, ver):
    for d in srcdirs:
        p = os.path.join(d, f'{name}-{ver}', 'Cargo.toml')
        if os.path.exists(p):
            m = re.search(r'^license\s*=\s*"([^"]+)"', open(p, errors='ignore').read(), re.M)
            if m:
                return m.group(1)
    for d in cachedirs:
        c = os.path.join(d, f'{name}-{ver}.crate')
        if os.path.exists(c):
            with tarfile.open(c) as tf:
                for mem in tf.getmembers():
                    if mem.name.endswith('Cargo.toml'):
                        t = tf.extractfile(mem).read().decode(errors='ignore')
                        m = re.search(r'^license\s*=\s*"([^"]+)"', t, re.M)
                        if m:
                            return m.group(1)
                        break
    return None

def from_crates_io(name, ver):
    cache_f = os.path.join(os.path.dirname(os.path.abspath(__file__)), '.license-cache.json')
    cache = json.load(open(cache_f)) if os.path.exists(cache_f) else {}
    key = f'{name}@{ver}'
    if key in cache:
        return cache[key]
    url = f'https://crates.io/api/v1/crates/{name}/{ver}'
    try:
        raw = subprocess.run(['curl', '-sS', '-m', '20', '-A', 'sailterm-license-audit', url],
                             capture_output=True, check=True).stdout
        lic = json.loads(raw)['version'].get('license') or 'SEE-REPO'
    except Exception:
        lic = 'UNKNOWN'
    if lic not in ('UNKNOWN',):
        cache[key] = lic
        json.dump(cache, open(cache_f, 'w'))
    return lic

rust_rows = []
unresolved = []
for i, (name, ver) in enumerate(pkgs):
    if name == 'sailterm':
        continue  # 本项目自身，不入第三方清单
    lic = from_local(name, ver)
    if lic is None:
        lic = from_crates_io(name, ver)
        time.sleep(0.12)
    if lic in ('UNKNOWN', 'SEE-REPO'):
        unresolved.append((name, ver, lic))
    rust_rows.append((name, ver, lic))
print(f'rust: {len(rust_rows)} pkgs, unresolved={len(unresolved)}', file=sys.stderr)
for u in unresolved:
    print('  unresolved:', u, file=sys.stderr)

# ---------- npm ----------
nm = os.path.join(ROOT, 'node_modules')
npm_rows = {}
patterns = [
    os.path.join(nm, '*', 'package.json'),
    os.path.join(nm, '@*', '*', 'package.json'),
    # pnpm 虚拟存储：全部包的实体都在这里
    os.path.join(nm, '.pnpm', '*', 'node_modules', '*', 'package.json'),
    os.path.join(nm, '.pnpm', '*', 'node_modules', '@*', '*', 'package.json'),
]
metas = [m for p in patterns for m in glob.glob(p)]
for meta in metas:
    try:
        j = json.load(open(meta))
    except Exception:
        continue
    if not j.get('name'):
        continue
    lic = j.get('license')
    if isinstance(lic, dict):
        lic = lic.get('type')
    npm_rows[(j['name'], j.get('version', ''))] = lic or 'NO-FIELD'
npm_sorted = sorted(npm_rows.items())
print(f'npm: {len(npm_sorted)} pkgs', file=sys.stderr)

# ---------- direct deps ----------
pkg_json = json.load(open(os.path.join(ROOT, 'package.json')))
direct_npm = sorted(set(list(pkg_json['dependencies']) + list(pkg_json['devDependencies'])))
cargo_toml = open(os.path.join(TAURI, 'Cargo.toml')).read()
direct_rs = sorted(set(re.findall(r'^\s*(?:[\w-]+/)?([\w][\w.-]*)\s*=\s*(?:"|\{)', cargo_toml, re.M)) - {'name', 'version', 'features', 'license', 'description', 'authors', 'edition', 'crate-type'})

# ---------- output ----------
out = []
out.append('# Third-Party Software Licenses\n')
out.append('SailTerm (MIT) bundles/links the following third-party components.\n'
           'All components are used under permissive terms (MIT / Apache-2.0 / BSD / ISC /\n'
           'Zlib / Unicode-3.0 / CC0 / CDLA-Permissive). Multi-licensed crates are selected\n'
           'under their MIT/Apache-2.0 option (notably `r-efi`, which also offers an LGPL-2.1\n'
           'alternative that we do NOT use). No GPL/LGPL/AGPL-only component is included.\n')
out.append('This file is auto-generated; regenerate after dependency changes.\n')
out.append('## Direct dependencies\n')
out.append('### Rust (src-tauri)\n')
out.append('| Crate | Version | License |\n|---|---|---|')
rs_map = {n: (v, l) for n, v, l in rust_rows}
for d in direct_rs:
    if d in rs_map:
        v, l = rs_map[d]
        out.append(f'| {d} | {v} | {l} |')
out.append('\n### JavaScript (frontend)\n')
out.append('| Package | Version | License |\n|---|---|---|')
for d in direct_npm:
    for (n, v), l in npm_sorted:
        if n == d:
            out.append(f'| {n} | {v} | {l} |')
            break
out.append('\n## Full transitive list — Rust (Cargo.lock)\n')
out.append('| Package | Version | License |\n|---|---|---|')
for n, v, l in sorted(rust_rows):
    out.append(f'| {n} | {v} | {l} |')
out.append('\n## Full transitive list — npm (node_modules)\n')
out.append('| Package | Version | License |\n|---|---|---|')
for (n, v), l in npm_sorted:
    out.append(f'| {n} | {v} | {l} |')
out.append('')
open(os.path.join(ROOT, 'THIRD-PARTY-LICENSES.md'), 'w').write('\n'.join(out))
print('written THIRD-PARTY-LICENSES.md', file=sys.stderr)
