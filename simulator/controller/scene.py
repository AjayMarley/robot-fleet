"""
Isaac Sim standalone scene: loads a Franka arm and streams joint telemetry
to robot-agent via Unix socket.

robot-agent (Rust) listens on $SOCKET_PATH. This scene connects to it and
sends length-prefixed joint frames each physics step. robot-agent encodes
them as TelemetryFrame proto and streams over gRPC to TelemetryService.

Run inside the container:
    /isaac-sim/kit/python/python3 controller/scene.py [--headless]
"""

import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--headless", action="store_true", help="Run without a display window")
args, _ = parser.parse_known_args()

# SimulationApp must be created before any other omni imports
from isaacsim import SimulationApp  # type: ignore  # noqa: E402

simulation_app = SimulationApp({
    "headless": args.headless,
    "width": 1280,
    "height": 720,
    "renderer": "RaytracedLighting",
})

import carb  # type: ignore  # noqa: E402
from omni.isaac.core import World  # type: ignore  # noqa: E402
from omni.isaac.core.robots import Robot  # type: ignore  # noqa: E402
from omni.isaac.core.utils.stage import add_reference_to_stage  # type: ignore  # noqa: E402
from omni.isaac.core.utils.nucleus import get_assets_root_path  # type: ignore  # noqa: E402

from backends import IsaacSimBackend  # noqa: E402
from robot_controller import RobotController, SOCKET_PATH, DIRECTIVE_SOCKET_PATH  # noqa: E402

FRANKA_ASSET_PATH = get_assets_root_path() + "/Isaac/Robots/Franka/franka.usd"
ROBOT_PRIM_PATH = "/World/Franka"


def main():
    world = World(stage_units_in_meters=1.0)
    world.scene.add_default_ground_plane()

    carb.log_info(f"Loading robot from: {FRANKA_ASSET_PATH}")
    add_reference_to_stage(usd_path=FRANKA_ASSET_PATH, prim_path=ROBOT_PRIM_PATH)
    robot = world.scene.add(Robot(prim_path=ROBOT_PRIM_PATH, name="franka"))
    world.reset()

    backend = IsaacSimBackend(robot)
    controller = RobotController(
        backend=backend,
        socket_path=SOCKET_PATH,
        directive_path=DIRECTIVE_SOCKET_PATH,
    )
    # robot-agent must be listening on SOCKET_PATH before initialize() is called
    controller.initialize()
    carb.log_info(f"Connected to robot-agent at {SOCKET_PATH} — streaming telemetry")

    step = 0
    while simulation_app.is_running():
        world.step(render=not args.headless)
        controller.on_physics_step(world.get_physics_dt())
        step += 1
        if step % 600 == 0:
            pos = robot.get_joint_positions()
            carb.log_info(f"[step {step}] joint[0]={pos[0]:.3f} rad")

    controller.shutdown()
    simulation_app.close()


if __name__ == "__main__":
    main()
