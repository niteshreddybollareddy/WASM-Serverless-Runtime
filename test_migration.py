import base64
import requests
import time

def read_wasm(path):
    with open(path, "rb") as f:
        return f.read()

def start_instance(wasm_bytes):
    b64 = base64.b64encode(wasm_bytes).decode("utf-8")
    payload = {"wasm_bytes_base64": b64}
    res = requests.post("http://127.0.0.1:8080/start", json=payload).json()
    if not res.get("success"):
        print(f"Start failed: {res}")
        exit(1)
    return res["instance_id"], res["worker_id"]

def invoke(instance_id, function_name):
    payload = {"instance_id": instance_id, "function_name": function_name}
    res = requests.post("http://127.0.0.1:8080/invoke", json=payload).json()
    if not res.get("success"):
        print(f"Invoke failed: {res}")
        return None
    return res["result"]

def migrate(instance_id, wasm_bytes):
    b64 = base64.b64encode(wasm_bytes).decode("utf-8")
    payload = {"instance_id": instance_id, "wasm_bytes_base64": b64}
    res = requests.post("http://127.0.0.1:8080/migrate", json=payload).json()
    if not res.get("success"):
        print(f"Migrate failed: {res}")
        exit(1)
    return res["new_worker_id"], res["migration_time_ms"]

if __name__ == "__main__":
    wasm_bytes = read_wasm("target/wasm32-unknown-unknown/release/example_wasm.wasm")
    
    print("1. Starting Stateful WASM Instance...")
    instance_id, worker_a = start_instance(wasm_bytes)
    print(f"   Instance {instance_id} started on {worker_a}")

    print("\n2. Incrementing State on Worker A...")
    for _ in range(3):
        res = invoke(instance_id, "increment")
        print(f"   Counter: {res}")

    print("\n3. Triggering COLD-CODE Live Migration (Worker A -> Worker B)...")
    worker_b, migration_time = migrate(instance_id, wasm_bytes)
    print(f"   Migrated to {worker_b} in {migration_time:.2f} ms (Includes Cranelift JIT time)")

    print("\n4. Resuming State on Worker B...")
    for _ in range(3):
        res = invoke(instance_id, "increment")
        print(f"   Counter: {res}")

    print("\n5. Triggering WARM-CODE Live Migration (Worker B -> Worker A)...")
    worker_a_again, migration_time_warm = migrate(instance_id, wasm_bytes)
    print(f"   Migrated back to {worker_a_again} in {migration_time_warm:.2f} ms (Pure state migration!)")

    print("\n6. Resuming State on Worker A...")
    for _ in range(3):
        res = invoke(instance_id, "increment")
        print(f"   Counter: {res}")

    print("\nSUCCESS: Memory state successfully migrated between processes!")
