use proto::controller_service_client::ControllerServiceClient;
use proto::worker_service_server::{WorkerService, WorkerServiceServer};
use proto::{
    HeartbeatResponse, InvokeRequest, InvokeResponse, RestoreRequest, RestoreResponse,
    SnapshotRequest, SnapshotResponse, StartInstanceRequest, StartInstanceResponse, WasmPayload,
    WasmResponse, WorkerInfo, WorkerLoad,
};
use std::collections::HashMap;
use std::sync::Arc;
use sysinfo::System;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Duration};
use tonic::{transport::Server, Request, Response, Status};
use wasmtime::*;
use tracing::{error, info};

struct ActiveInstance {
    store: Mutex<Store<()>>,
    instance: Instance,
}

#[derive(Clone)]
struct AppState {
    worker_id: String,
    engine: Engine,
    module_cache: Arc<RwLock<HashMap<String, Module>>>,
    active_instances: Arc<RwLock<HashMap<String, Arc<ActiveInstance>>>>,
}

#[derive(Clone)]
struct MyWorkerService {
    state: AppState,
}

#[tonic::async_trait]
impl WorkerService for MyWorkerService {
    async fn execute_wasm(
        &self,
        request: Request<WasmPayload>,
    ) -> Result<Response<WasmResponse>, Status> {
        // Keeping this for backwards compatibility
        Err(Status::unimplemented("Use start_instance and invoke"))
    }

    async fn start_instance(
        &self,
        request: Request<StartInstanceRequest>,
    ) -> Result<Response<StartInstanceResponse>, Status> {
        let req = request.into_inner();
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        req.wasm_bytes.hash(&mut hasher);
        let wasm_hash = hasher.finish().to_string();

        let module = {
            let cache = self.state.module_cache.read().await;
            if let Some(m) = cache.get(&wasm_hash) {
                m.clone()
            } else {
                drop(cache);
                let m = match Module::new(&self.state.engine, &req.wasm_bytes) {
                    Ok(m) => m,
                    Err(e) => return Ok(Response::new(StartInstanceResponse {
                        success: false,
                        instance_id: "".to_string(),
                        error_message: format!("Compilation failed: {}", e),
                    })),
                };
                let mut cache = self.state.module_cache.write().await;
                cache.insert(wasm_hash.clone(), m.clone());
                m
            }
        };

        let mut store = Store::new(&self.state.engine, ());
        
        let start_time = std::time::Instant::now();
        let instance = match Instance::new(&mut store, &module, &[]) {
            Ok(inst) => inst,
            Err(e) => return Ok(Response::new(StartInstanceResponse {
                success: false,
                instance_id: "".to_string(),
                error_message: format!("Instantiation failed: {}", e),
            })),
        };
        let instantiation_time = start_time.elapsed().as_nanos();
        info!("Cold Start Instantiation Time: {} ns ({} µs)", instantiation_time, instantiation_time as f64 / 1000.0);

        let instance_id = uuid::Uuid::new_v4().to_string();
        let active_instance = Arc::new(ActiveInstance {
            store: Mutex::new(store),
            instance,
        });

        self.state.active_instances.write().await.insert(instance_id.clone(), active_instance);

        info!("Started new instance: {}", instance_id);
        Ok(Response::new(StartInstanceResponse {
            success: true,
            instance_id,
            error_message: "".to_string(),
        }))
    }

    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let req = request.into_inner();
        let active = {
            let instances = self.state.active_instances.read().await;
            instances.get(&req.instance_id).cloned()
        };

        let active = match active {
            Some(a) => a,
            None => return Err(Status::not_found("Instance not found")),
        };

        let start_time = std::time::Instant::now();
        let mut store = active.store.lock().await;

        let func = match active.instance.get_typed_func::<(), i32>(&mut *store, &req.function_name) {
            Ok(f) => f,
            Err(e) => return Ok(Response::new(InvokeResponse {
                success: false,
                result: 0,
                error_message: format!("Function not found: {}", e),
                execution_time_ns: start_time.elapsed().as_nanos() as u64,
            })),
        };

        let exec_result = func.call(&mut *store, ());
        let execution_time_ns = start_time.elapsed().as_nanos() as u64;

        match exec_result {
            Ok(res) => Ok(Response::new(InvokeResponse {
                success: true,
                result: res,
                error_message: "".to_string(),
                execution_time_ns,
            })),
            Err(e) => Ok(Response::new(InvokeResponse {
                success: false,
                result: 0,
                error_message: format!("Trap: {}", e),
                execution_time_ns,
            })),
        }
    }

    async fn snapshot(
        &self,
        request: Request<SnapshotRequest>,
    ) -> Result<Response<SnapshotResponse>, Status> {
        let req = request.into_inner();
        let active = {
            let instances = self.state.active_instances.read().await;
            instances.get(&req.instance_id).cloned()
        };

        let active = match active {
            Some(a) => a,
            None => return Err(Status::not_found("Instance not found")),
        };

        let mut store = active.store.lock().await;
        
        // Export linear memory
        let memory = match active.instance.get_memory(&mut *store, "memory") {
            Some(m) => m,
            None => return Err(Status::internal("Instance has no memory exported")),
        };

        let data = memory.data(&*store).to_vec();

        // Also remove it from this worker, as it's migrating away
        self.state.active_instances.write().await.remove(&req.instance_id);
        info!("Snapshotted instance {}", req.instance_id);

        Ok(Response::new(SnapshotResponse {
            success: true,
            memory_bytes: data,
            error_message: "".to_string(),
        }))
    }

    async fn restore(
        &self,
        request: Request<RestoreRequest>,
    ) -> Result<Response<RestoreResponse>, Status> {
        let req = request.into_inner();
        let start_time = std::time::Instant::now();

        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        req.wasm_bytes.hash(&mut hasher);
        let wasm_hash = hasher.finish().to_string();

        let module = {
            let cache = self.state.module_cache.read().await;
            if let Some(m) = cache.get(&wasm_hash) {
                m.clone()
            } else {
                drop(cache);
                let m = match Module::new(&self.state.engine, &req.wasm_bytes) {
                    Ok(m) => m,
                    Err(e) => return Ok(Response::new(RestoreResponse {
                        success: false,
                        error_message: format!("Compilation failed: {}", e),
                        restore_time_ns: start_time.elapsed().as_nanos() as u64,
                    })),
                };
                let mut cache = self.state.module_cache.write().await;
                cache.insert(wasm_hash.clone(), m.clone());
                m
            }
        };

        let mut store = Store::new(&self.state.engine, ());
        let instance = match Instance::new(&mut store, &module, &[]) {
            Ok(inst) => inst,
            Err(e) => return Ok(Response::new(RestoreResponse {
                success: false,
                error_message: format!("Instantiation failed: {}", e),
                restore_time_ns: start_time.elapsed().as_nanos() as u64,
            })),
        };

        // Inject state
        let memory = match instance.get_memory(&mut store, "memory") {
            Some(m) => m,
            None => return Ok(Response::new(RestoreResponse {
                success: false,
                error_message: "Instance has no memory exported".to_string(),
                restore_time_ns: start_time.elapsed().as_nanos() as u64,
            })),
        };

        // Ensure memory is large enough
        let required_pages = (req.memory_bytes.len() as u64 + 65535) / 65536;
        let current_pages = memory.size(&store);
        if required_pages > current_pages {
            if let Err(e) = memory.grow(&mut store, required_pages - current_pages) {
                return Ok(Response::new(RestoreResponse {
                    success: false,
                    error_message: format!("Failed to grow memory: {}", e),
                    restore_time_ns: start_time.elapsed().as_nanos() as u64,
                }));
            }
        }

        if let Err(e) = memory.write(&mut store, 0, &req.memory_bytes) {
             return Ok(Response::new(RestoreResponse {
                success: false,
                error_message: format!("Failed to write memory: {}", e),
                restore_time_ns: start_time.elapsed().as_nanos() as u64,
            }));
        }

        let active_instance = Arc::new(ActiveInstance {
            store: Mutex::new(store),
            instance,
        });

        self.state.active_instances.write().await.insert(req.instance_id.clone(), active_instance);

        info!("Restored instance {}", req.instance_id);

        Ok(Response::new(RestoreResponse {
            success: true,
            error_message: "".to_string(),
            restore_time_ns: start_time.elapsed().as_nanos() as u64,
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let worker_id = uuid::Uuid::new_v4().to_string();
    
    // Enable Pooling Allocator for microsecond cold starts
    let mut pool_config = PoolingAllocationConfig::default();
    pool_config.memory_pages(160); // 10MB per instance (160 * 64KiB)
    pool_config.total_memories(100);
    pool_config.total_tables(100);
    pool_config.total_core_instances(100);

    let mut config = Config::new();
    config.wasm_multi_memory(true);
    config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool_config));
    
    let engine = Engine::new(&config)?;

    let state = AppState {
        worker_id: worker_id.clone(),
        engine,
        module_cache: Arc::new(RwLock::new(HashMap::new())),
        active_instances: Arc::new(RwLock::new(HashMap::new())),
    };

    let worker_service = MyWorkerService { state: state.clone() };

    let worker_port = std::env::var("WORKER_PORT").unwrap_or_else(|_| "50052".to_string());
    let worker_address = format!("[::1]:{}", worker_port);
    let public_address = format!("http://[::1]:{}", worker_port);
    let controller_address = "http://[::1]:50051".to_string();

    info!("Starting worker {} on {}", worker_id, worker_address);

    let mut client = match ControllerServiceClient::connect(controller_address.clone()).await {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to connect to controller at {}: {}", controller_address, e);
            std::process::exit(1);
        }
    };

    let register_req = tonic::Request::new(WorkerInfo {
        worker_id: worker_id.clone(),
        address: public_address,
    });

    match client.register_worker(register_req).await {
        Ok(_) => info!("Successfully registered with controller"),
        Err(e) => {
            error!("Failed to register with controller: {}", e);
            std::process::exit(1);
        }
    }

    let mut hb_client = client.clone();
    let hb_worker_id = worker_id.clone();
    tokio::spawn(async move {
        let mut sys = System::new_all();
        loop {
            sleep(Duration::from_secs(5)).await;
            sys.refresh_cpu();
            let cpu_load = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32;

            let hb_req = tonic::Request::new(WorkerLoad {
                worker_id: hb_worker_id.clone(),
                cpu_load,
            });

            if let Err(e) = hb_client.heartbeat(hb_req).await {
                error!("Failed to send heartbeat: {}", e);
            }
        }
    });

    let addr = worker_address.parse()?;
    Server::builder()
        .add_service(WorkerServiceServer::new(worker_service))
        .serve(addr)
        .await?;

    Ok(())
}
