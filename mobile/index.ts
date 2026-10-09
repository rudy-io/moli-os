// Background tasks must exist before anything else runs (the system may wake
// the app for them alone): src/moli.ts defines them when imported.
import "./src/moli";
import { registerRootComponent } from "expo";
import App from "./App";

registerRootComponent(App);
