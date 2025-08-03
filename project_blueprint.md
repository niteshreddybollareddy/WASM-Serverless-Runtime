# Project Blueprint: Distributed WebAssembly (WASM) Serverless Runtime

## 🎯 Goal
Architect a distributed edge-compute runtime capable of executing untrusted user code with microsecond cold starts. This simulates the core infrastructure behind platforms like Cloudflare Workers and Fastly Compute@Edge.

## 🛠 Tech Stack
- **Language:** Rust (for extreme memory safety and performance)
- **WASM Engine:** Wasmtime (Bytecode Alliance)
- **Networking/RPC:** gRPC (Tonic)
- **Concepts:** JIT (Just-In-Time) Compilation, Memory Sandboxing, Distributed Load Balancing

## ✅ Acceptance Criteria & Target Metrics

### 1. The Microsecond Cold-Start (The < 50µs Benchmark)
- **Target:** Achieve a cold-start execution time of **< 50 microseconds**.
- **Implementation:** Completely bypass containerization (like Docker). Instead, load pre-compiled `.wasm` binaries directly into memory.
- **Requirements:** Your engine must dynamically instantiate a WASM module, execute a function (like a simple HTTP handler), and return the result in under 50µs.

### 2. JIT Compilation & Sandboxing
- **Target:** Securely execute untrusted code without virtual machines.
- **Implementation:** Utilize the **Wasmtime** runtime with the **Cranelift** JIT compiler to translate WebAssembly bytecode into native machine code at runtime.
- **Requirements:** You must enforce strict Linear Memory isolation. The WASM guest code cannot be allowed to read or write to the host OS memory (demonstrate this by attempting a memory out-of-bounds error and cleanly catching it on the host).

### 3. Clustered Control Plane (gRPC)
- **Target:** Distributed execution across multiple worker nodes.
- **Implementation:** Build a Controller node and multiple Worker nodes that communicate via gRPC.
- **Requirements:** When a request comes into the Controller, it must dynamically route the WASM execution payload to the worker node with the lowest CPU load. 


