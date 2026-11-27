# Connect a DEEBOT X8 PRO OMNI

The robot must already be visible in ECOVACS HOME on an Italian account.
Add this to the Moli configuration and restart the service:

```toml
[[driver]]
id = "ecovacs"
kind = "ecovacs"

[driver.options]
writable = false
```

On the Moli host, run from the checkout:

```bash
bash scripts/pair-ecovacs.sh
```

Enter the email and password of the Ecovacs account at the terminal. When
Ecovacs requests verification, enter the code received by email. The script restarts
Moli after successful pairing. Check the robot's battery and state on
the dashboard. Repeat pairing if the session is revoked.

To enable movement controls, set `writable = true` and restart Moli. Commands
go through the hub's machine controls and guard. Test with the robot in a
neutral area and someone present, then return the robot to its original state.
