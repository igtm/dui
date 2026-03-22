use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn can_reach_docker_and_list_containers() {
    let docker = Docker::connect_with_defaults().expect("docker connects");
    let containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::default().all(true).build(),
        ))
        .await
        .expect("lists containers");

    assert!(containers.capacity() >= containers.len());
}
