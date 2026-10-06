{{- define "ferrfleet-runner.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else if contains .Chart.Name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{- define "ferrfleet-runner.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "ferrfleet-runner.labels" -}}
{{ include "ferrfleet-runner.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/component: runner
app.kubernetes.io/part-of: ferrfleet
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end }}

{{- define "ferrfleet-runner.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (.Values.image.tag | default .Chart.AppVersion) }}
{{- end }}
{{- end }}

{{- define "ferrfleet-runner.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- .Values.serviceAccount.name | default (include "ferrfleet-runner.fullname" .) }}
{{- else }}
{{- .Values.serviceAccount.name | default "default" }}
{{- end }}
{{- end }}

{{- define "ferrfleet-runner.poolTokenSecret" -}}
{{- .Values.poolToken.existingSecret | default (include "ferrfleet-runner.fullname" .) }}
{{- end }}

{{- define "ferrfleet-runner.claudeCredentialSecret" -}}
{{- .Values.claudeCredential.existingSecret | default (include "ferrfleet-runner.fullname" .) }}
{{- end }}

{{- define "ferrfleet-runner.apiUrl" -}}
{{- required "apiUrl is required" .Values.apiUrl | trimSuffix "/" }}
{{- end }}

{{- define "ferrfleet-runner.validate" -}}
{{- if not (has .Values.mode (list "scaledJob" "deployment")) }}
{{- fail (printf "mode must be scaledJob or deployment, got %q" .Values.mode) }}
{{- end }}
{{- if not (or .Values.poolToken.existingSecret .Values.poolToken.value) }}
{{- fail "set poolToken.existingSecret to a Secret holding the pool token, or poolToken.value" }}
{{- end }}
{{- if and .Values.poolToken.value (not (hasPrefix "ffrp_" .Values.poolToken.value)) }}
{{- fail "poolToken.value is not a pool token: it must start with ffrp_" }}
{{- end }}
{{- if not (or .Values.claudeCredential.existingSecret .Values.claudeCredential.value) }}
{{- fail "set claudeCredential.existingSecret to a Secret holding the Claude credential, or claudeCredential.value" }}
{{- end }}
{{- end }}

{{- define "ferrfleet-runner.podMetadata" -}}
labels:
  {{- include "ferrfleet-runner.labels" . | nindent 2 }}
  {{- with .Values.podLabels }}
  {{- toYaml . | nindent 2 }}
  {{- end }}
{{- with .Values.podAnnotations }}
annotations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}

{{- define "ferrfleet-runner.podSpec" -}}
{{- with .Values.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
serviceAccountName: {{ include "ferrfleet-runner.serviceAccountName" . }}
automountServiceAccountToken: {{ .Values.serviceAccount.automountToken }}
terminationGracePeriodSeconds: {{ .Values.terminationGracePeriodSeconds }}
securityContext:
  {{- toYaml .Values.podSecurityContext | nindent 2 }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
containers:
  - name: runner
    image: {{ include "ferrfleet-runner.image" . }}
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    args:
      - agent
      {{- if eq .Values.mode "scaledJob" }}
      - --ephemeral
      {{- end }}
    env:
      - name: HOME
        value: /tmp
      - name: FERRFLEET_WORKING_DIR
        value: /workdir
      - name: FERRFLEET_API_URL
        value: {{ include "ferrfleet-runner.apiUrl" . | quote }}
      - name: FERRFLEET_POOL_TOKEN
        valueFrom:
          secretKeyRef:
            name: {{ include "ferrfleet-runner.poolTokenSecret" . }}
            key: {{ .Values.poolToken.key }}
      - name: {{ .Values.claudeCredential.env }}
        valueFrom:
          secretKeyRef:
            name: {{ include "ferrfleet-runner.claudeCredentialSecret" . }}
            key: {{ .Values.claudeCredential.key }}
      {{- with .Values.extraEnv }}
      {{- toYaml . | nindent 6 }}
      {{- end }}
    resources:
      {{- toYaml .Values.resources | nindent 6 }}
    securityContext:
      {{- toYaml .Values.securityContext | nindent 6 }}
    volumeMounts:
      - name: workdir
        mountPath: /workdir
      - name: tmp
        mountPath: /tmp
volumes:
  - name: workdir
    emptyDir:
      sizeLimit: {{ .Values.workdirSizeLimit }}
  - name: tmp
    emptyDir:
      sizeLimit: {{ .Values.tmpSizeLimit }}
{{- end }}
