variable "aws_region" {
  description = "AWS region for the MVP deployment."
  type        = string
}

variable "aws_profile" {
  description = "Optional local AWS profile for plan/apply."
  type        = string
  default     = null
  nullable    = true
}

variable "name_prefix" {
  description = "Short prefix used for AWS resource names."
  type        = string
  default     = "ancilla"
}

variable "app_env" {
  description = "Ancilla runtime environment label."
  type        = string
  default     = "production"
}

variable "create_network" {
  description = "When true, create a dedicated MVP VPC, public app subnets, reserved private app subnets, and private DB subnets."
  type        = bool
  default     = true
}

variable "vpc_id" {
  description = "Existing VPC ID to use when create_network is false."
  type        = string
  default     = null
  nullable    = true
}

variable "public_subnet_ids" {
  description = "Existing public subnet IDs to use when create_network is false."
  type        = list(string)
  default     = []
}

variable "private_app_subnet_ids" {
  description = "Existing private application subnet IDs reserved for future private service deployments."
  type        = list(string)
  default     = []
}

variable "private_db_subnet_ids" {
  description = "Existing private database subnet IDs to use when create_network is false."
  type        = list(string)
  default     = []
}

variable "vpc_cidr" {
  description = "CIDR block for the MVP VPC when create_network is true."
  type        = string
  default     = "10.42.0.0/16"
}

variable "availability_zones" {
  description = "Availability zones for the created network. Use at least two AZs."
  type        = list(string)
  default     = ["us-east-1a", "us-east-1b"]
}

variable "public_subnet_cidrs" {
  description = "CIDR blocks for public subnets in the created network."
  type        = list(string)
  default     = ["10.42.0.0/24", "10.42.1.0/24"]
}

variable "private_app_subnet_cidrs" {
  description = "CIDR blocks for private application subnets reserved for future private service deployments."
  type        = list(string)
  default     = ["10.42.10.0/24", "10.42.11.0/24"]
}

variable "private_db_subnet_cidrs" {
  description = "CIDR blocks for private PostgreSQL subnets in the created network."
  type        = list(string)
  default     = ["10.42.20.0/24", "10.42.21.0/24"]
}

variable "allowed_ingress_cidr_blocks" {
  description = "CIDR blocks allowed to reach the public ECS task."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "domain_name" {
  description = "Optional public DNS name for Ancilla, for example ancillabot.com."
  type        = string
  default     = null
  nullable    = true
}

variable "create_www_record" {
  description = "When true and domain_name is set, also create www.<domain_name> pointing at the ALB."
  type        = bool
  default     = true
}

variable "create_api_record" {
  description = "When true and domain_name is set, also create api.<domain_name> pointing at the ALB."
  type        = bool
  default     = true
}

variable "enable_https_listener" {
  description = "When true, wait for ACM DNS validation and create an HTTPS listener plus HTTP->HTTPS redirect."
  type        = bool
  default     = false
}

variable "basic_auth_enabled" {
  description = "When true, require HTTP Basic auth for all app routes except /healthz."
  type        = bool
  default     = false
}

variable "basic_auth_username" {
  description = "Username to require when basic_auth_enabled is true."
  type        = string
  default     = "ancilla"
}

variable "app_port" {
  description = "Ancilla HTTP port inside the container."
  type        = number
  default     = 3000
}

variable "container_image" {
  description = "Optional full container image URI. Leave null to use the managed ECR repository URL plus tag."
  type        = string
  default     = null
  nullable    = true
}

variable "container_image_tag" {
  description = "Container image tag when using the managed ECR repository."
  type        = string
  default     = "latest"
}

variable "task_cpu" {
  description = "Fargate task CPU units."
  type        = number
  default     = 256
}

variable "task_memory" {
  description = "Fargate task memory in MiB."
  type        = number
  default     = 1024
}

variable "desired_count" {
  description = "Desired ECS task count."
  type        = number
  default     = 1
}

variable "log_retention_days" {
  description = "CloudWatch Logs retention for the app container."
  type        = number
  default     = 14
}

variable "bedrock_chat_model_id" {
  description = "Bedrock Converse model ID for Ancilla chat responses."
  type        = string
}

variable "bedrock_gate_model_id" {
  description = "Bedrock Converse model ID for Ancilla context gating. Leave null to let the server derive a default."
  type        = string
  default     = null
  nullable    = true
}

variable "bedrock_chat_models" {
  description = "Curated Bedrock chat models exposed to the TUI model picker."
  type = list(object({
    label                  = string
    model_id               = string
    description            = optional(string)
    thinking_mode          = optional(string)
    thinking_effort        = optional(string)
    thinking_budget_tokens = optional(number)
  }))
  default = []
}

variable "bedrock_chat_max_tokens" {
  description = "Default Bedrock max output tokens."
  type        = number
  default     = 800
}

variable "bedrock_chat_temperature" {
  description = "Default Bedrock temperature."
  type        = number
  default     = 0.2
}

variable "db_name" {
  description = "PostgreSQL database name."
  type        = string
  default     = "ancilla"
}

variable "db_username" {
  description = "PostgreSQL master username."
  type        = string
  default     = "ancilla"
}

variable "db_engine_version" {
  description = "RDS PostgreSQL engine version. Use a version that supports pgvector."
  type        = string
  default     = null
  nullable    = true
}

variable "db_instance_class" {
  description = "RDS PostgreSQL instance class."
  type        = string
  default     = null
  nullable    = true
}

variable "db_allocated_storage_gb" {
  description = "Allocated PostgreSQL storage in GB."
  type        = number
  default     = 20
}

variable "db_max_allocated_storage_gb" {
  description = "Upper bound for PostgreSQL storage autoscaling in GB."
  type        = number
  default     = 100
}

variable "db_storage_type" {
  description = "RDS PostgreSQL storage type."
  type        = string
  default     = "gp3"
}

variable "aurora_engine_version" {
  description = "Deprecated alias for db_engine_version."
  type        = string
  default     = null
  nullable    = true
}

variable "aurora_instance_class" {
  description = "Deprecated alias for db_instance_class."
  type        = string
  default     = null
  nullable    = true
}

variable "backup_retention_period" {
  description = "PostgreSQL backup retention period in days."
  type        = number
  default     = 1
}

variable "embedding_memory_model_id" {
  description = "Embedding model used for memory_records."
  type        = string
  default     = "perplexity-ai/pplx-embed-v1-0.6b"
}

variable "embedder_enabled" {
  description = "When true, create the always-on Ancilla embedder instance and point the server at it."
  type        = bool
  default     = true
}

variable "embedder_image" {
  description = "Optional full container image URI for the Ancilla embedder service."
  type        = string
  default     = null
  nullable    = true
}

variable "embedder_image_tag" {
  description = "Container image tag for the managed Ancilla embedder ECR repository."
  type        = string
  default     = "latest"
}

variable "embedder_instance_type" {
  description = "EC2 instance type for the always-on embedder host."
  type        = string
  default     = "g6f.large"
}

variable "embedder_accelerator" {
  description = "Accelerator mode for the embedder host. Use cpu for a low-cost fallback or gpu for CUDA-backed inference."
  type        = string
  default     = "gpu"

  validation {
    condition     = contains(["cpu", "gpu"], lower(var.embedder_accelerator))
    error_message = "embedder_accelerator must be either cpu or gpu."
  }
}

variable "embedder_root_volume_size_gb" {
  description = "Root volume size in GB for the embedder instance."
  type        = number
  default     = 80
}

variable "embedder_port" {
  description = "HTTP port exposed by the embedder service."
  type        = number
  default     = 4000
}

variable "embedder_timeout_seconds" {
  description = "Timeout in seconds for the server's embedder HTTP client."
  type        = number
  default     = 120
}

variable "embedder_device" {
  description = "Device preference passed into the embedder service."
  type        = string
  default     = "auto"
}

variable "embedder_batch_size" {
  description = "Batch size used by the embedder service."
  type        = number
  default     = 2
}

variable "embedder_max_length" {
  description = "Maximum tokenized input length passed to the embedder model."
  type        = number
  default     = 8192
}
