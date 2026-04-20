"""
Isaac Sim 5.x Script Editor — Unitree H1 humanoid scene.

Paste into: Window > Script Editor > Run

Prerequisites:
  - File > New  (clean stage before running)
  - robot-agent running in WSL2:
      TCP_LISTEN_ADDR=0.0.0.0:7777 ... cargo run -p robot-agent

Usage:
  1. Run this script (simulation stopped)
  2. Press Play
  3. H1 stands frozen upright; telemetry streams to robot-agent over TCP

Find WSL2 IP from Windows PowerShell:  wsl hostname -I
"""

import sys
import numpy as np
from pxr import UsdPhysics, Sdf
from omni.isaac.core import World
from omni.isaac.core.robots import Robot
from omni.isaac.core.utils.stage import add_reference_to_stage
from omni.isaac.core.utils.nucleus import get_assets_root_path
import omni.usd

# ── Config ────────────────────────────────────────────────────────────────────
ROBOT_AGENT_HOST = "127.0.0.1"   # WSL2 IP from: wsl hostname -I
ROBOT_AGENT_PORT = 7777
CONTROLLER_PATH  = r"C:\path\to\robot-fleet\simulator\controller"
# ─────────────────────────────────────────────────────────────────────────────

if World.instance() is not None:
    World.instance().clear_instance()

assets_root = get_assets_root_path()
world = World(stage_units_in_meters=1.0)
world.scene.add_default_ground_plane()
add_reference_to_stage(
    usd_path=assets_root + "/Isaac/Robots/Unitree/H1/h1.usd",
    prim_path="/World/H1",
)

stage = omni.usd.get_context().get_stage()
fixed_joint = UsdPhysics.FixedJoint.Define(stage, "/World/H1_world_joint")
fixed_joint.GetBody1Rel().SetTargets([Sdf.Path("/World/H1/pelvis")])

robot = world.scene.add(Robot(prim_path="/World/H1", name="h1"))
world.reset()
robot.set_world_pose(position=np.array([0.0, 0.0, 1.05]))

print(f"H1 loaded — {len(robot.dof_names)} DOF: {list(robot.dof_names)}")

# ── TCP telemetry streaming ───────────────────────────────────────────────────
sys.path.insert(0, CONTROLLER_PATH)

from backends import IsaacSimBackend
from robot_controller import RobotController

backend = IsaacSimBackend(robot)
ctrl = RobotController(
    backend=backend,
    socket_mode="tcp",
    tcp_host=ROBOT_AGENT_HOST,
    tcp_port=ROBOT_AGENT_PORT,
)

try:
    ctrl.initialize()
    print(f"Connected to robot-agent at {ROBOT_AGENT_HOST}:{ROBOT_AGENT_PORT}")

    def on_physics_step(dt):
        ctrl.on_physics_step(dt)

    world.add_physics_callback("h1_telemetry", on_physics_step)
    print("Telemetry callback registered — press Play to start streaming")

except OSError as e:
    print(f"[warn] robot-agent not reachable ({e}) — running without telemetry")
    print("Start robot-agent with TCP_LISTEN_ADDR=0.0.0.0:7777 then re-run")
