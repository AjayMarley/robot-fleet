"""
Isaac Sim standalone scene: loads a Franka arm, runs the fleet bootstrap flow,
then streams joint telemetry to robot-agent via the Unix socket bridge.

Run inside the container:
    /isaac-sim/kit/python/python3 controller/scene.py [--headless]
"""

import argparse
import os
import sys
import threading

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

import asyncio  # noqa: E402
import carb  # type: ignore  # noqa: E402
from omni.isaac.core import World  # type: ignore  # noqa: E402
from omni.isaac.core.robots import Robot  # type: ignore  # noqa: E402
from omni.isaac.core.utils.stage import add_reference_to_stage  # type: ignore  # noqa: E402
from omni.isaac.core.utils.nucleus import get_assets_root_path  # type: ignore  # noqa: E402

from robot_controller import RobotController, SOCKET_PATH, DIRECTIVE_SOCKET_PATH  # noqa: E402
from socket_bridge import SocketBridge  # noqa: E402

FRANKA_ASSET_PATH = (
    get_assets_root_path()
    + "/Isaac/Robots/Franka/franka.usd"
)
ROBOT_PRIM_PATH = "/World/Franka"


def _run_bridge(bridge: SocketBridge):
    """Run the async socket bridge in a dedicated thread."""
    asyncio.run(bridge.run())


def main():
    world = World(stage_units_in_meters=1.0)
    world.scene.add_default_ground_plane()

    # Load Franka from NVIDIA asset CDN
    carb.log_info(f"Loading robot from: {FRANKA_ASSET_PATH}")
    add_reference_to_stage(usd_path=FRANKA_ASSET_PATH, prim_path=ROBOT_PRIM_PATH)
    robot = world.scene.add(Robot(prim_path=ROBOT_PRIM_PATH, name="franka"))

    world.reset()

    # Start the Unix socket bridge (telemetry out, directives in)
    bridge = SocketBridge(
        telemetry_path=SOCKET_PATH,
        directive_path=DIRECTIVE_SOCKET_PATH,
    )
    bridge_thread = threading.Thread(target=_run_bridge, args=(bridge,), daemon=True)
    bridge_thread.start()
    carb.log_info(f"Socket bridge started — telemetry at {SOCKET_PATH}")

    # Attach our controller to the robot articulation
    controller = RobotController(
        socket_path=SOCKET_PATH,
        directive_path=DIRECTIVE_SOCKET_PATH,
    )
    # Override joint getters to read real Isaac Sim state
    controller.get_joint_positions = lambda: robot.get_joint_positions().tolist()
    controller.get_joint_velocities = lambda: robot.get_joint_velocities().tolist()

    carb.log_info("Scene ready — waiting for robot-agent to connect and enroll...")

    step = 0
    while simulation_app.is_running():
        world.step(render=not args.headless)
        controller.on_physics_step(world.get_physics_dt())
        step += 1
        if step % 600 == 0:  # log every ~10s at 60Hz
            pos = robot.get_joint_positions()
            carb.log_info(f"[step {step}] joint[0]={pos[0]:.3f} rad")

    controller.shutdown()
    simulation_app.close()


if __name__ == "__main__":
    main()
