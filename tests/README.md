# tests

Cross-component integration and end-to-end tests for InFMon.

Component-level unit tests live alongside the source under
`src/<component>/`. This directory is reserved for tests that exercise
multiple components together.

---

## E2E Tests

The E2E test suite lives under `tests/e2e/` and verifies InFMon's packet
processing pipeline end-to-end: traffic is replayed into the system and
flow counters are compared against known-good baselines.

### Prerequisites

| Requirement | Notes |
|-------------|-------|
| **BlueField-3 DPU** | Tests must run on the BF3 bench machine (e.g. `r12f-bf3`). |
| **VPP** | Running with the InFMon plugin loaded. |
| **Physical loopback** | A cable connecting TX and RX ports, **or** a remote host for TX. |
| **scapy** | `pip install scapy` (≥ 2.5). |
| **tcpreplay** | `apt install tcpreplay`. |
| **pytest** | `pip install pytest` (≥ 7.0). |

Install Python dependencies:

```bash
pip install -r tests/e2e/requirements.txt
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `INFMON_E2E_TX_MODE` | `local` | `local` (loopback cable) or `remote` (separate TX host). |
| `INFMON_E2E_TX_IFACE` | `p1` | Linux network interface used to transmit traffic. |
| `INFMON_E2E_TX_HOST` | *(empty)* | SSH-reachable hostname for remote TX mode. |
| `INFMON_E2E_TX_HOST_IFACE` | *(empty)* | Network interface on the remote TX host. |
| `INFMON_E2E_RX_VPP_IFACE` | `TwoHundredGigabitEthernet3/0/0` | VPP interface receiving traffic. |
| `INFMON_E2E_RX_IP` | `10.123.0.1/24` | IP address (CIDR notation) assigned to the VPP RX interface. The framework uses the prefix length for subnet configuration. |
| `INFMON_E2E_TX_IP` | `10.123.0.2/24` | IP address (CIDR notation) assigned to the TX interface. Pass the full CIDR form, not bare IP. |
| `INFMON_E2E_TEST_REFRESH_BASELINE` | `0` | Set to `1` to overwrite expected baselines with actual results. |

### How to Run

From the repo root:

```bash
# Run all E2E tests
make e2e

# Run with extra pytest arguments (e.g. a single scenario)
make e2e PYTEST_ARGS="-k erspan3_full"

# Refresh baselines (writes actual flow stats as the new expected values)
INFMON_E2E_TEST_REFRESH_BASELINE=1 make e2e
```

Or directly with pytest:

```bash
cd tests/e2e
python3 -m pytest -v --tb=short
```

### Local vs Remote TX Mode

- **Local mode** (`INFMON_E2E_TX_MODE=local`, default): Traffic is replayed
  from a Linux interface on the same machine, connected to the VPP RX port
  via a physical loopback cable.

- **Remote mode** (`INFMON_E2E_TX_MODE=remote`): Traffic is replayed from a
  separate host over SSH. Set `INFMON_E2E_TX_HOST` and
  `INFMON_E2E_TX_HOST_IFACE`. The test framework automatically copies
  replay scripts and scenario assets to the remote host via SCP.
  **Note:** passwordless SSH (key-based auth) to the remote host is
  required — otherwise SCP will hang waiting for a password prompt.

### How to Add a New Scenario

1. Create a directory under `tests/e2e/scenarios/` named after your scenario
   (e.g. `erspan3_new_feature/`).

2. Add an `input.pcap` file — this is the traffic that will be replayed.

3. Add an `expected_flows.json` file containing the expected flow counter
   output from InFMon after replaying the pcap. Use `{}` for scenarios
   where the packet should be dropped (invalid/truncated). To generate it
   automatically, run once with the refresh flag:

   ```bash
   INFMON_E2E_TEST_REFRESH_BASELINE=1 make e2e PYTEST_ARGS="-k new_feature"
   ```

4. *(Optional)* Add a `scenario.json` to configure flow rule fields and
   `max_keys`. If omitted, a default rule matching all traffic is created.

   ```json
   {
     "fields": {"src_ip": "10.0.0.1"},
     "max_keys": 100
   }
   ```

The test runner (`test_packet_replay.py`) automatically discovers all
scenario directories that contain both `input.pcap` and
`expected_flows.json`.

Golden PCAPs can be regenerated with `tests/e2e/gen_golden_pcaps.py`
(requires `scapy`).
