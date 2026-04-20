"""Simulator backend abstraction — decouples RobotController from any specific sim runtime."""

from __future__ import annotations

import math
import time
from abc import ABC, abstractmethod


JOINT_NAMES = ["panda_joint1", "panda_joint2", "panda_joint3",
               "panda_joint4", "panda_joint5", "panda_joint6"]


class SimulatorBackend(ABC):
    """Common interface implemented by every simulator backend."""

    @abstractmethod
    def get_joint_names(self) -> list[str]: ...

    @abstractmethod
    def get_joint_positions(self) -> list[float]: ...

    @abstractmethod
    def get_joint_torques(self) -> list[float]: ...


# ---------------------------------------------------------------------------
# Sine-wave stub — used in tests and when no simulator is present
# ---------------------------------------------------------------------------

class SineWaveBackend(SimulatorBackend):
    """Synthetic backend that produces sine-wave joint data without any simulator."""

    def __init__(self, joint_names: list[str] = JOINT_NAMES):
        self._names = joint_names

    def get_joint_names(self) -> list[str]:
        return self._names

    def get_joint_positions(self) -> list[float]:
        t = time.time()
        n = len(self._names)
        return [math.sin(t + i * math.pi / n) for i in range(n)]

    def get_joint_torques(self) -> list[float]:
        t = time.time()
        n = len(self._names)
        # Torque proportional to cosine — simulates spring-like restoring force
        return [0.5 * math.cos(t + i * math.pi / n) for i in range(n)]


# ---------------------------------------------------------------------------
# Isaac Sim backend
# ---------------------------------------------------------------------------

class IsaacSimBackend(SimulatorBackend):
    """
    Wraps an omni.isaac.core Robot articulation.
    Constructed in scene.py after World.reset() so the robot is fully initialised.
    """

    def __init__(self, robot):
        # robot: omni.isaac.core.robots.Robot
        self._robot = robot

    def get_joint_names(self) -> list[str]:
        return list(self._robot.dof_names)

    def get_joint_positions(self) -> list[float]:
        return self._robot.get_joint_positions().tolist()

    def get_joint_torques(self) -> list[float]:
        actions = self._robot.get_applied_action()
        efforts = actions.joint_efforts
        if efforts is None:
            return [0.0] * len(self.get_joint_names())
        return efforts.tolist()


# ---------------------------------------------------------------------------
# Webots backend
# ---------------------------------------------------------------------------

class WebotsBackend(SimulatorBackend):
    """
    Wraps a Webots Robot controller.
    Import is deferred so this module loads cleanly outside Webots.

    Usage in a Webots controller script:
        from controller import Robot
        wb_robot = Robot()
        backend = WebotsBackend(wb_robot, joint_names=[...])
        ctrl = RobotController(backend)
        timestep = int(wb_robot.getBasicTimeStep())
        while wb_robot.step(timestep) != -1:
            ctrl.on_physics_step(timestep / 1000.0)
    """

    def __init__(self, robot, joint_names: list[str] = JOINT_NAMES):
        self._robot = robot
        self._names = joint_names
        self._motors = [robot.getDevice(name) for name in joint_names]
        self._sensors = []
        for motor in self._motors:
            sensor = motor.getPositionSensor()
            sensor.enable(int(robot.getBasicTimeStep()))
            self._sensors.append(sensor)
        for motor in self._motors:
            motor.enableTorqueFeedback(int(robot.getBasicTimeStep()))

    def get_joint_names(self) -> list[str]:
        return self._names

    def get_joint_positions(self) -> list[float]:
        return [s.getValue() for s in self._sensors]

    def get_joint_torques(self) -> list[float]:
        return [m.getTorqueFeedback() for m in self._motors]
