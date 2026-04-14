data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}

module "network" {
  source = "./modules/network"
  count  = var.create_network ? 1 : 0

  name_prefix              = var.name_prefix
  aws_region               = var.aws_region
  app_env                  = var.app_env
  vpc_cidr                 = var.vpc_cidr
  availability_zones       = var.availability_zones
  public_subnet_cidrs      = var.public_subnet_cidrs
  private_app_subnet_cidrs = var.private_app_subnet_cidrs
  private_db_subnet_cidrs  = var.private_db_subnet_cidrs
}

locals {
  service_name           = "${var.name_prefix}-mvp"
  ecr_image              = "${aws_ecr_repository.app.repository_url}:${var.container_image_tag}"
  container_image        = coalesce(var.container_image, local.ecr_image)
  vpc_id                 = var.create_network ? module.network[0].vpc_id : var.vpc_id
  public_subnet_ids      = var.create_network ? module.network[0].public_subnet_ids : var.public_subnet_ids
  private_app_subnet_ids = var.create_network ? module.network[0].private_app_subnet_ids : var.private_app_subnet_ids
  private_db_subnet_ids  = var.create_network ? module.network[0].private_db_subnet_ids : var.private_db_subnet_ids
  db_engine_version      = coalesce(var.db_engine_version, var.aurora_engine_version, "15.8")
  db_instance_class      = coalesce(var.db_instance_class, var.aurora_instance_class, "db.t4g.micro")
  database_url           = "postgres://${var.db_username}:${random_password.db_password.result}@${aws_db_instance.postgres.address}:${aws_db_instance.postgres.port}/${var.db_name}?sslmode=require"
  bucket_name            = "${var.name_prefix}-${data.aws_caller_identity.current.account_id}-${var.aws_region}-memory-assets"
  tags = {
    Project     = "ancilla"
    Environment = var.app_env
    ManagedBy   = "terraform-or-tofu"
  }
}

resource "random_password" "db_password" {
  length  = 32
  special = false
}

resource "aws_ecr_repository" "app" {
  name                 = local.service_name
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }
}

resource "aws_cloudwatch_log_group" "app" {
  name              = "/ecs/${local.service_name}"
  retention_in_days = var.log_retention_days
}

resource "aws_s3_bucket" "assets" {
  bucket = local.bucket_name
}

resource "aws_s3_bucket_versioning" "assets" {
  bucket = aws_s3_bucket.assets.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "assets" {
  bucket = aws_s3_bucket.assets.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "assets" {
  bucket                  = aws_s3_bucket.assets.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_security_group" "ecs_service" {
  name        = "${local.service_name}-ecs"
  description = "Ancilla MVP public ECS tasks"
  vpc_id      = local.vpc_id

  ingress {
    from_port   = var.app_port
    to_port     = var.app_port
    protocol    = "tcp"
    cidr_blocks = var.allowed_ingress_cidr_blocks
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "db" {
  name        = "${local.service_name}-db"
  description = "Ancilla MVP PostgreSQL"
  vpc_id      = local.vpc_id

  ingress {
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.ecs_service.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_db_subnet_group" "postgres" {
  name       = "${local.service_name}-db-subnets"
  subnet_ids = local.private_db_subnet_ids
}

resource "aws_db_instance" "postgres" {
  identifier                      = "${local.service_name}-postgres"
  engine                          = "postgres"
  engine_version                  = local.db_engine_version
  instance_class                  = local.db_instance_class
  db_name                         = var.db_name
  username                        = var.db_username
  password                        = random_password.db_password.result
  allocated_storage               = var.db_allocated_storage_gb
  max_allocated_storage           = var.db_max_allocated_storage_gb
  storage_type                    = var.db_storage_type
  db_subnet_group_name            = aws_db_subnet_group.postgres.name
  vpc_security_group_ids          = [aws_security_group.db.id]
  backup_retention_period         = var.backup_retention_period
  storage_encrypted               = true
  deletion_protection             = false
  skip_final_snapshot             = true
  copy_tags_to_snapshot           = true
  enabled_cloudwatch_logs_exports = ["postgresql"]
  publicly_accessible             = false
}

resource "aws_secretsmanager_secret" "database_url" {
  name = "${local.service_name}/database-url"
}

resource "aws_secretsmanager_secret_version" "database_url" {
  secret_id     = aws_secretsmanager_secret.database_url.id
  secret_string = local.database_url
}

data "aws_iam_policy_document" "ecs_task_assume_role" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "ecs_execution" {
  name               = "${local.service_name}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_task_assume_role.json
}

resource "aws_iam_role_policy_attachment" "ecs_execution_managed" {
  role       = aws_iam_role.ecs_execution.name
  policy_arn = "arn:${data.aws_partition.current.partition}:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

data "aws_iam_policy_document" "ecs_execution_extra" {
  statement {
    sid     = "ReadDatabaseSecret"
    actions = ["secretsmanager:GetSecretValue"]
    resources = [
      aws_secretsmanager_secret.database_url.arn,
    ]
  }
}

resource "aws_iam_role_policy" "ecs_execution_extra" {
  name   = "${local.service_name}-execution-extra"
  role   = aws_iam_role.ecs_execution.id
  policy = data.aws_iam_policy_document.ecs_execution_extra.json
}

resource "aws_iam_role" "ecs_task" {
  name               = "${local.service_name}-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_task_assume_role.json
}

data "aws_iam_policy_document" "ecs_task" {
  statement {
    sid = "InvokeBedrock"
    actions = [
      "bedrock:InvokeModel",
      "bedrock:InvokeModelWithResponseStream",
    ]
    resources = ["*"]
  }

  statement {
    sid = "AccessAncillaAssetsBucket"
    actions = [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
    ]
    resources = ["${aws_s3_bucket.assets.arn}/*"]
  }

  statement {
    sid       = "ListAncillaAssetsBucket"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.assets.arn]
  }
}

resource "aws_iam_role_policy" "ecs_task" {
  name   = "${local.service_name}-task"
  role   = aws_iam_role.ecs_task.id
  policy = data.aws_iam_policy_document.ecs_task.json
}

resource "aws_ecs_cluster" "app" {
  name = local.service_name
}

resource "aws_ecs_task_definition" "app" {
  family                   = local.service_name
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.task_cpu)
  memory                   = tostring(var.task_memory)
  execution_role_arn       = aws_iam_role.ecs_execution.arn
  task_role_arn            = aws_iam_role.ecs_task.arn

  runtime_platform {
    cpu_architecture        = "ARM64"
    operating_system_family = "LINUX"
  }

  container_definitions = jsonencode([
    {
      name      = "ancilla"
      image     = local.container_image
      essential = true
      portMappings = [
        {
          containerPort = var.app_port
          hostPort      = var.app_port
          protocol      = "tcp"
        }
      ]
      environment = [
        { name = "ANCILLA_APP_ENV", value = var.app_env },
        { name = "AWS_REGION", value = var.aws_region },
        { name = "AWS_DEFAULT_REGION", value = var.aws_region },
        { name = "BEDROCK_CHAT_MODEL_ID", value = var.bedrock_chat_model_id },
        { name = "BEDROCK_CHAT_MODELS_JSON", value = jsonencode(var.bedrock_chat_models) },
        { name = "BEDROCK_CHAT_MAX_TOKENS", value = tostring(var.bedrock_chat_max_tokens) },
        { name = "BEDROCK_CHAT_TEMPERATURE", value = tostring(var.bedrock_chat_temperature) }
      ]
      secrets = [
        {
          name      = "DATABASE_URL"
          valueFrom = aws_secretsmanager_secret.database_url.arn
        }
      ]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.app.name
          awslogs-region        = var.aws_region
          awslogs-stream-prefix = "ancilla"
        }
      }
    }
  ])
}

resource "aws_ecs_service" "app" {
  name                               = local.service_name
  cluster                            = aws_ecs_cluster.app.id
  task_definition                    = aws_ecs_task_definition.app.arn
  desired_count                      = var.desired_count
  launch_type                        = "FARGATE"
  enable_execute_command             = true
  deployment_maximum_percent         = 200
  deployment_minimum_healthy_percent = 50

  network_configuration {
    subnets          = local.public_subnet_ids
    security_groups  = [aws_security_group.ecs_service.id]
    assign_public_ip = true
  }
}
