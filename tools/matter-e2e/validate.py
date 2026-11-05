"""Real Moli REST assertions. Failures never turn into fixture success."""
import json, os, pathlib, subprocess, time, urllib.request
D = pathlib.Path(os.environ['E2E_DIR'])
B = os.environ['MOLI_BIN']
os.environ['MOLI_MASTER_KEY'] = '11' * 32  # disposable synthetic test key
os.environ['MOLI_UI_PIN'] = '112233'

def wait(check, label, seconds=90):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            value = check()
            if value: return value
        except (OSError, ValueError, KeyError): pass
        time.sleep(.5)
    raise AssertionError(f'timeout: {label}; inspect {D}/*.log')

def api(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request('http://127.0.0.1:8790/api/' + path, data=data,
        headers={'Content-Type':'application/json', 'X-Moli-Origin':'cli'})
    with urllib.request.urlopen(req, timeout=3) as r:
        assert r.status == 200, r.status
        return json.load(r)

def cli(*args, value=None, ok=True):
    r = subprocess.run([B, '--config', str(D/'moli.toml'), *args], input=value,
                       text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if ok: assert r.returncode == 0, r.stderr
    return r

def start(label):
    return subprocess.Popen([B, '--config', str(D/'moli.toml'), 'serve'],
                            stdout=(D/f'{label}.log').open('w'), stderr=subprocess.STDOUT)
def stop(p):
    p.terminate()
    try: p.wait(timeout=10)
    except subprocess.TimeoutExpired: p.kill(); p.wait()

def device():
    return next((d for d in api('devices')['devices'] if d['instance']=='matter'), None)

def state(expected):
    d = device()
    return d and d['online'] is True and d['state'].get('on',{}).get('value') is expected

def command(identity, value):
    api('command', {'point': identity+'/on', 'value':value})
    wait(lambda:state(value), f'Moli subscription on={value}')
    wait(lambda:json.loads((D/'accessory-state.json').read_text())['on'] is value,
         'independent accessory state')

try:
    wait(lambda:(D/'accessory-ready').exists(), 'virtual accessory startup', 30)
    (D/'moli.toml').write_text(f'''[server]
listen = "127.0.0.1:8790"
data_dir = "{D}/moli"
[[driver]]
id = "matter"
kind = "matter"
[driver.options]
paa_roots = "{D}/paa"
cd_roots = "{D}/cd"
''')
    cli('check-config')
    cli('secrets','set','matter','pairing_code',value=(D/'pairing-code').read_text())
    p = start('commission')
    try:
        d = wait(device, 'commissioning and Descriptor discovery', 120)
        identity = d['id']
        wait(lambda:state(False), 'initial subscription')
        command(identity, True)
        command(identity, False)
    finally: stop(p)
    p = start('restart')
    try:
        wait(lambda:state(False), 'restart CASE and subscription', 120)
        assert device()['id'] == identity, 'device identity changed'
        command(identity, True)
        command(identity, False)
    finally: stop(p)
    print('PASS: commissioning, descriptor, OnOff, subscription, restart identity', flush=True)
    # CLI overwrites only this disposable vault's controller state.
    for label, snapshot in [('invalid-hex','not-hex'), ('invalid-snapshot','00')]:
        cli('secrets','set','matter','controller_snapshot',value=snapshot)
        p = start(label)
        try:
            wait(lambda:api('health'), 'API startup after corrupt snapshot', 30)
            drivers = wait(lambda: next((d for d in api('devices')['drivers']
                if d['instance']=='matter' and 'backoff' in json.dumps(d).lower()), None),
                'driver rejects '+label, 15)
            (D/f'{label}-driver.json').write_text(json.dumps(drivers))
            d = device()
            assert d is None or d['online'] is not True, 'corrupt snapshot online'
            try:
                api('command', {'point':identity+'/on', 'value':True})
            except urllib.error.HTTPError as e:
                assert e.code >= 400
            else:
                raise AssertionError('corrupt state accepted command')
        finally: stop(p)
    os.environ['MOLI_MASTER_KEY'] = '22' * 32
    assert cli('secrets','set','matter','pairing_code', value='synthetic', ok=False).returncode != 0
    print('PASS: malformed state, invalid snapshot, wrong vault key rejected', flush=True)
except Exception as e:
    print(f'FAIL: {e}', flush=True)
    raise
