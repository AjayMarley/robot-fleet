FROM nvcr.io/nvidia/isaac-sim:6.0.0-dev2

WORKDIR /workspace/simulator

RUN /isaac-sim/kit/python/python3 -m pip install pytest pytest-asyncio --quiet

COPY controller/ ./controller/
COPY testdata/  ./testdata/

ENV SOCKET_PATH=/tmp/robot_telemetry.sock
ENV DIRECTIVE_SOCKET_PATH=/tmp/robot_directive.sock

# Default: run unit tests (no GPU display needed)
# Override CMD to launch the visual scene:
#   docker run ... robot-fleet-simulator /isaac-sim/kit/python/python3 controller/scene.py
CMD ["/isaac-sim/kit/python/python3", "-m", "pytest", "testdata/", "-m", "not gpu", "-v"]
