import base64
import requests
import time

def read_wasm(path):
    with open(path, "rb") as f:
        return f.read()

def execute(function_name, wasm_bytes):
    b64 = base64.b64encode(wasm_bytes).decode("utf-8")
    payload = {
        "function_name": function_name,
        "wasm_bytes_base64": b64
    }
    
    start = time.time()
    try:
        response = requests.post("http://127.0.0.1:8080/execute", json=payload)
        end = time.time()
        print(f"[{function_name}] Status: {response.status_code}")
        if response.status_code == 200:
            res = response.json()
            print(f"Success: {res.get('success')}")
            print(f"Message: {res.get('message')}")
            print(f"Result: {res.get('result')}")
            print(f"Execution Time (ns): {res.get('execution_time_ns')} ns ({res.get('execution_time_ns', 0) / 1000.0} us)")
            print(f"Worker ID: {res.get('worker_id')}")
            print(f"End-to-End Time: {(end - start) * 1000.0:.2f} ms")
            print("-" * 40)
        else:
            print(response.text)
    except Exception as e:
        print(f"Request failed: {e}")

if __name__ == "__main__":
    wasm_bytes = read_wasm("target/wasm32-unknown-unknown/release/example_wasm.wasm")
    
    print("Testing 'run' (Normal function for cold start)...")
    # Call multiple times to see caching effect and benchmark
    for i in range(3):
        print(f"Call {i+1}:")
        execute("run", wasm_bytes)
        time.sleep(0.1)

    print("\nTesting 'crash' (Out-of-bounds memory trap)...")
    execute("crash", wasm_bytes)
