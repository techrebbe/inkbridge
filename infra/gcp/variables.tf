variable "enable_deployment" {
  description = "Hard opt-in. False creates a zero-resource plan."
  type        = bool
  default     = false
}

variable "deployment_acknowledgement" {
  description = "Must equal the documented acknowledgement before resources can be enabled."
  type        = string
  default     = ""
}

variable "project_id" {
  description = "Existing Google Cloud project ID."
  type        = string
  default     = "inkbridge-not-configured"
}

variable "project_number" {
  description = "Numeric project number, used for the Cloud Storage service agent IAM grant."
  type        = string
  default     = "000000000000"
}

variable "region" {
  description = "One region shared by the bucket, Eventarc trigger, Cloud Run, and Firestore."
  type        = string
  default     = "us-central1"
}

variable "bucket_name" {
  description = "Globally unique private Cloud Storage bucket name."
  type        = string
  default     = "inkbridge-not-configured"
}

variable "cloud_run_image" {
  description = "Immutable linux/amd64 container image digest for the runtime."
  type        = string
  default     = "us-docker.pkg.dev/cloudrun/container/hello"
}

variable "firestore_database" {
  description = "Firestore Native database ID."
  type        = string
  default     = "(default)"
}

variable "monthly_budget_usd" {
  description = "Optional billing budget. Zero omits it. Budgets alert but do not cap charges."
  type        = number
  default     = 0
}

variable "billing_account" {
  description = "Billing account ID required only when monthly_budget_usd is non-zero."
  type        = string
  default     = ""
  sensitive   = true
}
