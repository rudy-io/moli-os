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

def api(path, body=None, origin="cli", status=200, cookie=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request('http://127.0.0.1:8790/api/' + path, data=data,
        headers={'Content-Type':'application/json', 'X-Moli-Origin':origin, **({'Cookie':cookie} if cookie else {})})
    with urllib.request.urlopen(req, timeout=3) as r:
        assert r.status == status, (r.status, status)
        return json.load(r)

def cli(*args, value=None, ok=True):
    r = subprocess.run([B, '--config', str(D/'moli.toml'), *args], input=value,
                       text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
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
    return endpoint('light')

def state(expected):
    d = device()
    return d and d['online'] is True and d['state'].get('on',{}).get('value') is expected

def command(identity, value):
    api('command', {'point': identity+'/on', 'value':value})
    wait(lambda:state(value), f'Moli subscription on={value}')
    wait(lambda:json.loads((D/'accessory-state.json').read_text())['on'] is value,
         'independent accessory state')

def endpoint(name):
    number = json.loads((D/'endpoints.json').read_text())[name]
    return next((d for d in api('devices')['devices']
        if d['instance']=='matter' and d['id'].endswith('-'+str(number))), None)

def observed():
    return json.loads((D/'extended-state.json').read_text())

def points(d):
    return {p['key']:p for p in d['points']}

def reading(name, key):
    d = endpoint(name)
    return d and d['online'] is True and d['state'].get(key,{}).get('value')

def extended():
    if os.environ.get('E2E_EXTENDED') != '1': return
    for name in ['color','cover','thermostat','lock','readonly-lock']:
        wait(lambda:endpoint(name), 'Descriptor '+name, 30)
    (D/'discovered.json').write_text(json.dumps(api('devices'),indent=2))
    # Metadata derives from each real server, never fabricated feature maps.
    assert not {'brightness','color','color_temp'} & points(endpoint('light')).keys()
    assert 'control' not in points(endpoint('readonly-lock')), 'disabled actuator writable'
    assert not points(endpoint('lock'))['locked']['access']['write']
    assert not points(endpoint('lock'))['lock_state']['access']['write']

    human_cookie = None

    def send(name, key, value, state_key, expected, wire_key=None, wire_value=None, command=None, approve=False):
        point = points(endpoint(name)).get(key)
        assert point and point['access']['write'], f'{name}/{key}: metadata not writable'
        before = observed()
        point_id = endpoint(name)['id']+'/'+key
        result = api('command', {'point':point_id,'value':value},status=202 if approve else 200)
        if approve:
            assert observed()['commands'] == before['commands'], 'held CLI command transmitted before approval'
            api('approvals/'+str(result['pending']), {'approve':True,'point':point_id},origin='ui',cookie=human_cookie)
        wait(lambda:reading(name,state_key)==expected, f'{name}/{key}: Moli subscription {expected}', 20)
        if wire_key:
            wait(lambda:any(e['endpoint']==name and e['key']==wire_key and e['value']==wire_value
                for e in observed()['events'][len(before['events']):]),
                f'{name}/{key}: fresh independent accessory event', 10)
        if command:
            wait(lambda:any(c['endpoint']==name and c['command']==command
                for c in observed()['commands'][len(before['commands']):]),
                f'{name}/{key}: accessory command {command}', 10)

    def lighting():
        send('color','brightness',75,'brightness',191/254*100,'currentLevel',191)
        send('color','brightness',50,'brightness',50.0,'currentLevel',127)
        send('color','color_temp',5000,'color_temp',5000.0,'colorTemperatureMireds',200)
        send('color','color_temp',4000,'color_temp',4000.0,'colorTemperatureMireds',250)
        print('PASS: dimming and CT wire state + subscriptions',flush=True)
        # XY conversion normalizes brightness; accept the small wire quantization error.
        before = observed()
        api('command', {'point':endpoint('color')['id']+'/color','value':'#ff0000'})
        wait(lambda: observed()['states'].get('color.currentX') == 45915 and
             observed()['states'].get('color.currentY') == 19615, 'RGB -> XY independent wire state', 20)
        def red():
            value = reading('color','color')
            (D/'rgb-snapshot.json').write_text(json.dumps(endpoint('color'),indent=2))
            return isinstance(value,str) and len(value)==7 and int(value[1:3],16)>=250 and max(int(value[3:5],16),int(value[5:7],16))<=5
        wait(red, 'RGB subscription red', 20)
        assert len(observed()['events']) > len(before['events']), 'no fresh color event'
        print('PASS: extended lighting dimming, CT, RGB wire state + subscriptions',flush=True)

    def covering():
        send('cover','position',37.25,'position',37.25,'currentPositionLiftPercent100ths',6275,'position')
        send('cover','cover_control','open','position',100.0,'currentPositionLiftPercent100ths',0,'open')
        send('cover','cover_control','close','position',0.0,'currentPositionLiftPercent100ths',10000,'close')
        send('cover','cover_control','stop','position',0.0,command='stop')
        send('cover','position',50,'position',50.0,'currentPositionLiftPercent100ths',5000,'position')
        print('PASS: covering inverted position, open/close/stop + subscriptions',flush=True)

    def thermostat():
        send('thermostat','heat_setpoint',21.25,'heat_setpoint',21.25,'occupiedHeatingSetpoint',2125)
        send('thermostat','cool_setpoint',27.5,'cool_setpoint',27.5,'occupiedCoolingSetpoint',2750)
        for mode, wire in [('heat',4),('cool',3),('auto',1),('off',0)]:
            send('thermostat','mode',mode,'mode',mode,'systemMode',wire)
        send('thermostat','heat_setpoint',20,'heat_setpoint',20.0,'occupiedHeatingSetpoint',2000)
        send('thermostat','cool_setpoint',26,'cool_setpoint',26.0,'occupiedCoolingSetpoint',2600)
        print('PASS: thermostat absolute heat/cool, modes + subscriptions',flush=True)

    def lock():
        nonlocal human_cookie
        # All values and this human session belong only to the disposable fixture.
        request = urllib.request.Request('http://127.0.0.1:8790/api/session',
            data=json.dumps({'pin':os.environ['MOLI_UI_PIN']}).encode(),
            headers={'Content-Type':'application/json'})
        with urllib.request.urlopen(request,timeout=3) as response:
            assert json.load(response)['human'] is True
            human_cookie = response.headers['Set-Cookie'].split(';',1)[0]
        assert 'control' in points(endpoint('lock')), 'lock/control: metadata gate rejected honest server policy'
        for origin in ['agent','api']:
            before = observed()
            api('command', {'point':endpoint('lock')['id']+'/control','value':'unlock'},origin=origin,status=202)
            time.sleep(1)
            assert observed()==before, f'{origin} approval transmitted to accessory'
            assert reading('lock','locked') is True
        send('lock','control','unlock','locked',False,'lockState',2,'unlock',approve=True)
        send('lock','control','lock','locked',True,'lockState',1,'lock',approve=True)
        print('PASS: lock approval does not transmit; approved CLI timed lock/unlock + subscription',flush=True)
        for seq, require_pin in [(1, True), (2, False)]:
            request = D/'policy-request.tmp'
            request.write_text(json.dumps({'seq':seq,'requirePin':require_pin}))
            request.replace(D/'policy-request.json')
            wait(lambda:json.loads((D/'policy-reply.json').read_text()) ==
                 {'seq':seq,'requirePin':require_pin}, 'accessory PIN policy IPC ack', 10)
            def capability():
                d = endpoint('lock')
                control = points(d).get('control') if d else None
                return d and d['online'] is True and bool(control and control['access']['write']) == (not require_pin)
            wait(capability, f'lock reactive metadata requirePin={require_pin}', 20)
            if require_pin:
                before = observed()['commands']
                try:
                    api('command', {'point':endpoint('lock')['id']+'/control','value':'unlock'})
                except urllib.error.HTTPError as error:
                    assert 400 <= error.code < 500, error.code
                else:
                    raise AssertionError('PIN-required lock accepted credential-free command')
                time.sleep(1)
                assert observed()['commands'] == before, 'PIN-required policy transmitted a command'
                assert reading('lock','locked') is True
        send('lock','control','unlock','locked',False,'lockState',2,'unlock',approve=True)
        send('lock','control','lock','locked',True,'lockState',1,'lock',approve=True)
        print('PASS: real accessory PIN policy withdraws/restores control; refusal does not transmit',flush=True)


    failures = []
    for phase in [lighting, covering, thermostat, lock]:
        try:
            phase()
        except Exception as error:
            failures.append(f'{phase.__name__}: {error}')
            print('FAIL: '+failures[-1],flush=True)
            (D/(phase.__name__+'-failure-devices.json')).write_text(json.dumps(api('devices'),indent=2))
    return failures

def measure_idle(process):
    if os.environ.get('E2E_MEASURE') != '1': return
    samples = []
    end = time.monotonic() + 30
    while time.monotonic() < end:
        assert process.poll() is None, 'Moli exited during measurement'
        status = dict(line.split(':',1) for line in
            pathlib.Path(f'/proc/{process.pid}/status').read_text().splitlines() if ':' in line)
        fields = pathlib.Path(f'/proc/{process.pid}/stat').read_text().rsplit(')',1)[1].split()
        samples.append((time.monotonic(), int(fields[11])+int(fields[12]),
            int(status['VmRSS'].split()[0]), int(status['RssAnon'].split()[0])))
        time.sleep(.25)
    first, last = samples[0], samples[-1]
    result = {'scope':('extended virtual families' if os.environ.get('E2E_EXTENDED') == '1' else 'one virtual light') + '; process memory, not cgroup; no BLE/OTBR',
        'seconds':round(last[0]-first[0],2),
        'rss_kib_peak':max(s[2] for s in samples),
        'anonymous_kib_peak':max(s[3] for s in samples),
        'cpu_percent_one_core':round(100*(last[1]-first[1]) /
            os.sysconf('SC_CLK_TCK') / (last[0]-first[0]),3)}
    (D/'measurement.json').write_text(json.dumps(result, indent=2))
    print('MEASURE: '+json.dumps(result), flush=True)

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
        failures = extended() or []
    finally: stop(p)
    p = start('restart')
    try:
        wait(lambda:state(False), 'restart CASE and subscription', 120)
        assert device()['id'] == identity, 'device identity changed'
        measure_idle(p)
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
    if failures: raise AssertionError('; '.join(failures))
except Exception as e:
    print(f'FAIL: {e}', flush=True)
    raise
