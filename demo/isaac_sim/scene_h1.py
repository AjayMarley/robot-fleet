"""
Isaac Sim 5.x Script Editor — Unitree H1 humanoid scene.

Paste into: Window > Script Editor > Run

Prerequisites:
  - File > New  (clean stage before running)
  - tcp_frame_listener.py running in WSL2 (for pipe test)
    OR robot-agent running with TCP_LISTEN_ADDR=0.0.0.0:7777

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
ROBOT_AGENT_HOST = "192.168.50.155"
ROBOT_AGENT_PORT = 7777
CONTROLLER_PATH  = r"\\wsl.localhost\Ubuntu-24.04\home\majay\code\robot-fleet\simulator\controller"
# ─────────────────────────────────────────────────────────────────────────────

stage = omni.usd.get_context().get_stage()
assets_root = get_assets_root_path()

# Reuse existing World or create one — never clear_instance() from Script Editor
world = World.instance() or World(stage_units_in_meters=1.0)

# Add H1 only if not already in stage
if not stage.GetPrimAtPath("/World/H1").IsValid():
    world.scene.add_default_ground_plane()
    add_reference_to_stage(
        usd_path=assets_root + "/Isaac/Robots/Unitree/H1/h1.usd",
        prim_path="/World/H1",
    )
    fixed_joint = UsdPhysics.FixedJoint.Define(stage, "/World/H1_world_joint")
    fixed_joint.GetBody1Rel().SetTargets([Sdf.Path("/World/H1/pelvis")])
    print("H1 added to stage")
else:
    print("H1 already in stage — skipping load")

robot = world.scene.get_object("h1") or world.scene.add(Robot(prim_path="/World/H1", name="h1"))
world.reset()
robot.set_world_pose(position=np.array([0.0, 0.0, 1.05]))
print(f"H1 ready — {len(robot.dof_names)} DOF: {list(robot.dof_names)}")

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

# Remove stale callback if re-running the script
world.remove_physics_callback("h1_telemetry")

try:
    ctrl.initialize()
    print(f"Connected to robot-agent at {ROBOT_AGENT_HOST}:{ROBOT_AGENT_PORT}")

    def on_physics_step(dt):
        ctrl.on_physics_step(dt)

    world.add_physics_callback("h1_telemetry", on_physics_step)
    print("Telemetry callback registered — press Play to start streaming")

except OSError as e:
    print(f"[warn] robot-agent not reachable ({e}) — running without telemetry")
    print("Start tcp_frame_listener.py in WSL2 first, then re-run this script")
