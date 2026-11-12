"""
Reference Python script equivalent to src/main.rs for OAK-D Lite (RGB + Stereo Depth).
"""
import cv2
import depthai as dai
import numpy as np

STREAM_WIDTH = 640
STREAM_HEIGHT = 400
MAX_DEPTH_MM = 3000.0


def main():
    with dai.Pipeline() as pipeline:
        # 1. Center RGB Camera (CAM_A)
        cam_rgb = pipeline.create(dai.node.Camera).build(dai.CameraBoardSocket.CAM_A)
        rgb_out = cam_rgb.requestOutput(
            (STREAM_WIDTH, STREAM_HEIGHT),
            dai.ImgFrame.Type.BGR888i,
            dai.ImgResizeMode.CROP,
            30.0,
        )

        # 2. Left (CAM_B) and Right (CAM_C) Mono Cameras + StereoDepth
        left = pipeline.create(dai.node.Camera).build(dai.CameraBoardSocket.CAM_B)
        right = pipeline.create(dai.node.Camera).build(dai.CameraBoardSocket.CAM_C)
        stereo = pipeline.create(dai.node.StereoDepth)

        left.requestFullResolutionOutput().link(stereo.left)
        right.requestFullResolutionOutput().link(stereo.right)

        # 3. Create Output Queues
        q_rgb = rgb_out.createOutputQueue()
        q_depth = stereo.depth.createOutputQueue()

        pipeline.start()
        print("Streaming OAK-D Lite RGB + Stereo Depth... Press 'q' to quit.")

        while pipeline.isRunning():
            in_rgb = q_rgb.tryGet()
            in_depth = q_depth.tryGet()

            if in_rgb is not None:
                rgb_frame = in_rgb.getCvFrame()
                cv2.imshow("OAK-D Lite - RGB (CamA)", rgb_frame)

            if in_depth is not None:
                depth_frame = in_depth.getFrame()  # uint16 in millimeters
                h, w = depth_frame.shape
                center_mm = depth_frame[h // 2, w // 2]

                # Normalize and colorize depth map
                depth_vis = np.clip(depth_frame, 0, MAX_DEPTH_MM)
                depth_vis = ((1.0 - depth_vis / MAX_DEPTH_MM) * 255).astype(np.uint8)
                depth_color = cv2.applyColorMap(depth_vis, cv2.COLORMAP_JET)
                depth_color[depth_frame == 0] = 0

                cv2.putText(
                    depth_color,
                    f"Center: {center_mm} mm",
                    (15, 30),
                    cv2.FONT_HERSHEY_SIMPLEX,
                    0.8,
                    (255, 255, 255),
                    2,
                )
                cv2.imshow("OAK-D Lite - Stereo Depth (CamB + CamC)", depth_color)

            if cv2.waitKey(1) == ord("q"):
                break


if __name__ == "__main__":
    main()

