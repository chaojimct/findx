"""Local version consistency and fail-closed publication gate. No network/writes."""
import argparse, json, re, sys, tomllib
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
def validate(root=ROOT, expected=None, require_ready=False):
    errors=[]
    workspace=tomllib.loads((root/'Cargo.toml').read_text(encoding='utf8'))
    version=workspace['workspace']['package']['version']
    if expected and expected != version: errors.append(f'tag/input {expected} != workspace {version}')
    for member in workspace['workspace']['members']:
        data=tomllib.loads((root/member/'Cargo.toml').read_text(encoding='utf8'))
        if data['package']['version'] != {'workspace':True}: errors.append(f'{member}: must inherit workspace version')
    for file in ['gui/package.json','gui/src-tauri/tauri.conf.json']:
        if json.loads((root/file).read_text(encoding='utf8'))['version'] != version: errors.append(f'{file}: version mismatch')
    lock=json.loads((root/'gui/package-lock.json').read_text(encoding='utf8'))
    if lock['version'] != version or lock['packages']['']['version'] != version: errors.append('npm lock root version mismatch')
    iss=(root/'installer/FindX.iss').read_text(encoding='utf8')
    if not re.search(r'#define MyAppVersion "'+re.escape(version)+'"',iss): errors.append('installer version mismatch')
    native=tomllib.loads((root/'crates/findx2-gui-native/Cargo.toml').read_text(encoding='utf8'))
    if 'pinyin' not in native['features']['default']: errors.append('native default pinyin disabled')
    plan=json.loads((root/'release'/f'{version}.json').read_text(encoding='utf8'))
    if plan['version'] != version or plan['windows_gui'] != 'native': errors.append('release plan version/backend mismatch')
    if require_ready:
        blocked=[g['id'] for g in plan['gates'] if g['required'] and not g['passed']]
        if not plan['publish_ready']: errors.append('publish_ready=false: local candidate only')
        errors.extend('unpassed upgrade gate: '+g for g in blocked)
    return version,errors
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--expected-version');p.add_argument('--require-ready',action='store_true');a=p.parse_args()
    version,errors=validate(expected=a.expected_version,require_ready=a.require_ready)
    print(json.dumps(dict(version=version,ok=not errors,errors=errors),indent=2))
    sys.exit(bool(errors))
