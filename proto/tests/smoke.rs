#[test]
fn fleet_types_compile() {
    let _ = robot_fleet_proto::fleet::v1::DeviceInfo::default();
    let _ = robot_fleet_proto::artifacts::v1::ArtifactMeta::default();
}
