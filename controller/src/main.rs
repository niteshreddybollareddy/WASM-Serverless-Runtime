use axum::{
    extract::{State, Json},
    routing::post,
    Router,
};
use proto::controller_service_server::{ControllerService, ControllerServiceServer};
use proto::worker_service_client::WorkerServiceClient;
use proto::{
    HeartbeatResponse, RegisterResponse, WorkerInfo, WorkerLoad,
    StartInstanceRequest, InvokeRequest, SnapshotRequest, RestoreRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{info, error};
use base64::Engine;

#[derive(Clone)]
struct WorkerState {
    address: String,
    cpu_load: f32,
    last_heartbeat: std::time::Instant,
}

#[derive(Clone)]
struct AppState {
    workers: Arc<RwLock<HashMap<String, WorkerState>>>,
    // Mapping of instance_id -> worker_id
    instance_locations: Arc<RwLock<HashMap<String, String>>>,
}

struct MyControllerService {
    state: AppState,
}

#[tonic::async_trait]
impl ControllerService for MyControllerService {
    async fn register_worker(
        &self,
        request: Request<WorkerInfo>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let req = request.into_inner();
        let mut workers = self.state.workers.write().await;
        workers.insert(
            req.worker_id.clone(),
            WorkerState {
                address: req.address.clone(),
                cpu_load: 0.0,
                last_heartbeat: std::time::Instant::now(),
            },
        );
        info!("Worker registered: {} at {}", req.worker_id, req.address);
        Ok(Response::new(RegisterResponse { success: true }))
    }

    async fn heartbeat(
        &self,
        request: Request<WorkerLoad>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();
        let mut workers = self.state.workers.write().await;
        if let Some(worker) = workers.get_mut(&req.worker_id) {
            worker.cpu_load = req.cpu_load;
            worker.last_heartbeat = std::time::Instant::now();
        } else {
            return Err(Status::unauthenticated("Worker not registered"));
        }
        Ok(Response::new(HeartbeatResponse { acknowledged: true }))
    }
}

// HTTP DTOs
#[derive(Deserialize)]
struct StartRequestHttp {
    wasm_bytes_base64: String,
}
#[derive(Serialize)]
struct StartResponseHttp {
    success: bool,
    instance_id: String,
    worker_id: String,
    error: String,
}

#[derive(Deserialize)]
struct InvokeRequestHttp {
    instance_id: String,
    function_name: String,
}
#[derive(Serialize)]
struct InvokeResponseHttp {
    success: bool,
    result: i32,
    execution_time_ns: u64,
    error: String,
}

#[derive(Deserialize)]
struct MigrateRequestHttp {
    instance_id: String,
    wasm_bytes_base64: String, // Need the WASM bytes to re-instantiate on new node
}
#[derive(Serialize)]
struct MigrateResponseHttp {
    success: bool,
    new_worker_id: String,
    migration_time_ms: f64,
    error: String,
}

// Handlers
async fn handle_start(
    State(state): State<AppState>,
    Json(payload): Json<StartRequestHttp>,
) -> Json<StartResponseHttp> {
    let wasm_bytes = match base64::engine::general_purpose::STANDARD.decode(&payload.wasm_bytes_base64) {
        Ok(b) => b,
        Err(e) => return Json(StartResponseHttp { success: false, instance_id: "".into(), worker_id: "".into(), error: e.to_string() }),
    };

    let worker_opt = {
        let workers = state.workers.read().await;
        workers.iter().min_by(|a, b| a.1.cpu_load.partial_cmp(&b.1.cpu_load).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, w)| (id.clone(), w.address.clone()))
    };
    
    let (worker_id, worker_address) = match worker_opt {
        Some(w) => w,
        None => return Json(StartResponseHttp { success: false, instance_id: "".into(), worker_id: "".into(), error: "No workers".into() }),
    };

    let mut client = WorkerServiceClient::connect(worker_address).await.unwrap();
    let res = client.start_instance(Request::new(StartInstanceRequest { wasm_bytes })).await.unwrap().into_inner();
    
    if res.success {
        state.instance_locations.write().await.insert(res.instance_id.clone(), worker_id.clone());
    }

    Json(StartResponseHttp {
        success: res.success,
        instance_id: res.instance_id,
        worker_id,
        error: res.error_message,
    })
}

async fn handle_invoke(
    State(state): State<AppState>,
    Json(payload): Json<InvokeRequestHttp>,
) -> Json<InvokeResponseHttp> {
    let worker_id = state.instance_locations.read().await.get(&payload.instance_id).cloned();
    let worker_id = match worker_id {
        Some(id) => id,
        None => return Json(InvokeResponseHttp { success: false, result: 0, execution_time_ns: 0, error: "Instance not found".into() }),
    };

    let worker_address = state.workers.read().await.get(&worker_id).unwrap().address.clone();
    let mut client = WorkerServiceClient::connect(worker_address).await.unwrap();

    let res = client.invoke(Request::new(InvokeRequest {
        instance_id: payload.instance_id,
        function_name: payload.function_name,
    })).await.unwrap().into_inner();

    Json(InvokeResponseHttp {
        success: res.success,
        result: res.result,
        execution_time_ns: res.execution_time_ns,
        error: res.error_message,
    })
}

async fn handle_migrate(
    State(state): State<AppState>,
    Json(payload): Json<MigrateRequestHttp>,
) -> Json<MigrateResponseHttp> {
    let start_time = std::time::Instant::now();
    let source_worker_id = state.instance_locations.read().await.get(&payload.instance_id).cloned();
    let source_worker_id = match source_worker_id {
        Some(id) => id,
        None => return Json(MigrateResponseHttp { success: false, new_worker_id: "".into(), migration_time_ms: 0.0, error: "Instance not found".into() }),
    };

    // Find destination worker (different from source, lowest load)
    let dest_worker_opt = {
        let workers = state.workers.read().await;
        workers.iter()
            .filter(|(id, _)| **id != source_worker_id)
            .min_by(|a, b| a.1.cpu_load.partial_cmp(&b.1.cpu_load).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, w)| (id.clone(), w.address.clone()))
    };

    let (dest_worker_id, dest_worker_address) = match dest_worker_opt {
        Some(w) => w,
        None => return Json(MigrateResponseHttp { success: false, new_worker_id: "".into(), migration_time_ms: 0.0, error: "No destination workers available".into() }),
    };

    let source_worker_address = state.workers.read().await.get(&source_worker_id).unwrap().address.clone();

    // 1. Snapshot
    let mut src_client = WorkerServiceClient::connect(source_worker_address).await.unwrap();
    let snap_res = src_client.snapshot(Request::new(SnapshotRequest { instance_id: payload.instance_id.clone() })).await.unwrap().into_inner();
    
    if !snap_res.success {
        return Json(MigrateResponseHttp { success: false, new_worker_id: "".into(), migration_time_ms: 0.0, error: snap_res.error_message });
    }

    // 2. Restore
    let wasm_bytes = base64::engine::general_purpose::STANDARD.decode(&payload.wasm_bytes_base64).unwrap();
    let mut dest_client = WorkerServiceClient::connect(dest_worker_address).await.unwrap();
    
    let restore_res = dest_client.restore(Request::new(RestoreRequest {
        instance_id: payload.instance_id.clone(),
        wasm_bytes,
        memory_bytes: snap_res.memory_bytes,
    })).await.unwrap().into_inner();

    if !restore_res.success {
        return Json(MigrateResponseHttp { success: false, new_worker_id: "".into(), migration_time_ms: 0.0, error: restore_res.error_message });
    }

    // Update location
    state.instance_locations.write().await.insert(payload.instance_id, dest_worker_id.clone());

    let migration_time_ms = start_time.elapsed().as_secs_f64() * 1000.0;
    info!("Migration took {:.2} ms", migration_time_ms);

    Json(MigrateResponseHttp {
        success: true,
        new_worker_id: dest_worker_id,
        migration_time_ms,
        error: "".into(),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let state = AppState {
        workers: Arc::new(RwLock::new(HashMap::new())),
        instance_locations: Arc::new(RwLock::new(HashMap::new())),
    };

    let grpc_state = state.clone();

    let http_state = state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/start", post(handle_start))
            .route("/invoke", post(handle_invoke))
            .route("/migrate", post(handle_migrate))
            .with_state(http_state);
        
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 8080));
        info!("HTTP API listening on {}", addr);
        axum::Server::bind(&addr).serve(app.into_make_service()).await.unwrap();
    });

    let addr = "[::1]:50051".parse()?;
    let controller_service = MyControllerService { state: grpc_state };
    info!("Controller gRPC server listening on {}", addr);

    Server::builder()
        .add_service(ControllerServiceServer::new(controller_service))
        .serve(addr)
        .await?;

    Ok(())
}
