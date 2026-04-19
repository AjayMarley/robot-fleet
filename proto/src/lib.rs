// Re-export prost-generated modules so dependents import from one place:
//   use robot_fleet_proto::fleet::v1::DeviceInfo;
//   use robot_fleet_proto::artifacts::v1::ArtifactMeta;

pub mod fleet {
    pub mod v1 {
        tonic::include_proto!("fleet.v1");
    }
}

pub mod artifacts {
    pub mod v1 {
        tonic::include_proto!("artifacts.v1");
    }
}
