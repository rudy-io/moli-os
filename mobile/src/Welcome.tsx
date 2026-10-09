import { useState } from "react";
import {
  ActivityIndicator,
  KeyboardAvoidingView,
  Linking,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { StatusBar } from "expo-status-bar";
import { normalize, probe, type Probe } from "./house";
import { t } from "./texts";

const BG = "#161512";
const GOLD = "#E8B931";
const REPO = "https://github.com/rudy-io/moli-os";

function explain(p: Exclude<Probe, { ok: true }>): string {
  switch (p.why) {
    case "refused":
      return t.refused;
    case "insecure":
      return t.insecure;
    case "not-moli":
      return t.notMoli;
    default:
      return t.unreachable;
  }
}

/** First screen: which house to open (like Home Assistant's server address). */
export function Welcome({ initial, onDone }: { initial?: string | null; onDone: (base: string) => void }) {
  const [text, setText] = useState(initial ?? "");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<{ message: string; base?: string } | null>(null);

  const submit = async () => {
    const base = normalize(text);
    if (!base) {
      setProblem({ message: t.badAddress });
      return;
    }
    setBusy(true);
    setProblem(null);
    const answer = await probe(base);
    setBusy(false);
    if (answer.ok) {
      onDone(base);
      return;
    }
    // Unencrypted over the Internet: never offered. Otherwise the person
    // knows their house better than a knock does (another sign-in in front…).
    setProblem({ message: explain(answer), base: answer.why === "insecure" ? undefined : base });
  };

  return (
    <SafeAreaView style={styles.root}>
      <StatusBar style="light" />
      <KeyboardAvoidingView style={styles.body} behavior={Platform.OS === "ios" ? "padding" : undefined}>
        <Text style={styles.title}>{t.welcomeTitle}</Text>
        <Text style={styles.text}>{t.welcomeText}</Text>
        <Text style={styles.label}>{t.addressLabel}</Text>
        <TextInput
          style={styles.input}
          value={text}
          onChangeText={(v) => {
            setText(v);
            setProblem(null);
          }}
          placeholder={t.addressPlaceholder}
          placeholderTextColor="#6f685c"
          autoCapitalize="none"
          autoCorrect={false}
          autoComplete="url"
          keyboardType="url"
          textContentType="URL"
          returnKeyType="go"
          onSubmitEditing={submit}
          editable={!busy}
          accessibilityLabel={t.addressLabel}
        />
        {problem && <Text style={styles.problem}>{problem.message}</Text>}
        <Pressable style={[styles.go, busy && styles.dim]} onPress={submit} disabled={busy} accessibilityRole="button">
          {busy ? (
            <View style={styles.row}>
              <ActivityIndicator color={BG} />
              <Text style={styles.goText}>{t.checking}</Text>
            </View>
          ) : (
            <Text style={styles.goText}>{t.connect}</Text>
          )}
        </Pressable>
        {problem?.base && (
          <Pressable style={styles.anyway} onPress={() => onDone(problem.base!)} accessibilityRole="button">
            <Text style={styles.anywayText}>{t.openAnyway}</Text>
          </Pressable>
        )}
        <Pressable onPress={() => Linking.openURL(REPO).catch(() => {})} style={styles.help}>
          <Text style={styles.helpText}>{t.help}</Text>
        </Pressable>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: BG },
  body: { flex: 1, justifyContent: "center", paddingHorizontal: 24 },
  title: { color: "#f3efe6", fontSize: 28, fontWeight: "700" },
  text: { color: "#cfc8ba", fontSize: 15, lineHeight: 21, marginTop: 10 },
  label: { color: "#b9b2a4", fontSize: 13, fontWeight: "600", marginTop: 28, marginBottom: 8 },
  input: {
    color: "#f3efe6",
    fontSize: 17,
    borderWidth: 1,
    borderColor: "#3a362f",
    backgroundColor: "#22201c",
    borderRadius: 14,
    paddingHorizontal: 16,
    paddingVertical: 14,
  },
  problem: { color: "#f0a58a", fontSize: 14, lineHeight: 20, marginTop: 12 },
  go: { marginTop: 20, backgroundColor: GOLD, borderRadius: 999, paddingVertical: 14, alignItems: "center" },
  dim: { opacity: 0.8 },
  row: { flexDirection: "row", alignItems: "center", gap: 10 },
  goText: { color: BG, fontSize: 16, fontWeight: "700" },
  anyway: { marginTop: 12, alignItems: "center", paddingVertical: 10 },
  anywayText: { color: GOLD, fontSize: 15, fontWeight: "600" },
  help: { marginTop: 32, alignItems: "center" },
  helpText: { color: "#8a8376", fontSize: 13, textAlign: "center", lineHeight: 18 },
});
