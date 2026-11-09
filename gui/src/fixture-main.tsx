import { createRoot } from "react-dom/client";
import { UpdateLayoutFixture } from "./findx/dev/UpdateLayoutFixture";

document.title = "FindX 模拟更新布局（不是生产包）";
createRoot(document.getElementById("root")!).render(<UpdateLayoutFixture />);
